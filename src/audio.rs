//! Always-on audio engine: mic, desktop (WASAPI loopback) and per-application audio.
//! It runs even when not recording so the level meters work; when a recording is active a
//! TCP sink is attached and the mixed 48 kHz stereo s16le stream is sent to ffmpeg.
//! The mixer is driven by the wall clock so silent devices never stall the stream.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Host, SampleFormat, Stream, SupportedStreamConfig};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;

/// Per-channel mixer controls shared with the UI.
pub struct Chan {
    gain: AtomicU32,
    muted: AtomicBool,
    peak: AtomicU32,
}

impl Chan {
    pub fn new(gain: f32) -> Self {
        Self { gain: AtomicU32::new(gain.to_bits()), muted: AtomicBool::new(false), peak: AtomicU32::new(0) }
    }
    pub fn gain(&self) -> f32 { f32::from_bits(self.gain.load(Ordering::Relaxed)) }
    pub fn set_gain(&self, g: f32) { self.gain.store(g.to_bits(), Ordering::Relaxed) }
    pub fn muted(&self) -> bool { self.muted.load(Ordering::Relaxed) }
    pub fn set_muted(&self, m: bool) { self.muted.store(m, Ordering::Relaxed) }
    pub fn peak(&self) -> f32 { f32::from_bits(self.peak.load(Ordering::Relaxed)) }
    fn set_peak(&self, p: f32) { self.peak.store(p.to_bits(), Ordering::Relaxed) }
}

/// What the engine should capture. `Some("")` = the default device.
#[derive(Clone, PartialEq, Default)]
pub struct Setup {
    pub mic: Option<String>,
    pub desk: Option<String>,
    pub app: Option<u32>, // process id
}

impl Setup {
    pub fn any(&self) -> bool {
        self.mic.is_some() || self.desk.is_some() || self.app.is_some()
    }
}

pub enum Cmd {
    Setup(Setup),
    Attach(TcpStream),
    Detach,
}

pub struct Engine {
    tx: Sender<Cmd>,
    pub err: Arc<Mutex<Option<String>>>,
}

impl Engine {
    pub fn new(mic: Arc<Chan>, desk: Arc<Chan>, app: Arc<Chan>) -> Self {
        let (tx, rx) = channel();
        let err = Arc::new(Mutex::new(None));
        let e2 = err.clone();
        thread::spawn(move || engine_loop(rx, [mic, desk, app], e2));
        Self { tx, err }
    }
    pub fn setup(&self, s: Setup) { let _ = self.tx.send(Cmd::Setup(s)); }
    pub fn sender(&self) -> Sender<Cmd> { self.tx.clone() }
}

/// (input device names, output device names). Done on a helper thread to keep COM happy.
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    thread::spawn(|| {
        let host = cpal::default_host();
        let names = |it: Result<Vec<Device>, _>| -> Vec<String> {
            it.map(|v: Vec<Device>| v.iter().filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
        };
        (
            names(host.input_devices().map(|i| i.collect())),
            names(host.output_devices().map(|i| i.collect())),
        )
    })
    .join()
    .unwrap_or_default()
}

type Queue = Arc<Mutex<VecDeque<f32>>>;

struct Src {
    q: Queue,
    ch: Arc<Chan>,
    _stream: Option<Stream>,
    stop: Option<Arc<AtomicBool>>,
}

impl Drop for Src {
    fn drop(&mut self) {
        if let Some(s) = &self.stop { s.store(true, Ordering::Relaxed); }
    }
}

struct Resampler { step: f64, pos: f64, prev: [f32; 2] }

impl Resampler {
    fn new(rate: u32) -> Self { Self { step: rate as f64 / RATE as f64, pos: 0.0, prev: [0.0; 2] } }
    fn process(&mut self, data: &[f32], ch: usize, q: &mut VecDeque<f32>) {
        for fr in data.chunks_exact(ch) {
            let cur = if ch == 1 { [fr[0], fr[0]] } else { [fr[0], fr[1]] };
            while self.pos < 1.0 {
                let t = self.pos as f32;
                q.push_back(self.prev[0] + (cur[0] - self.prev[0]) * t);
                q.push_back(self.prev[1] + (cur[1] - self.prev[1]) * t);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
        cap(q);
    }
}

fn cap(q: &mut VecDeque<f32>) {
    let extra = q.len().saturating_sub(RATE as usize * 2);
    if extra > 0 { q.drain(..extra); }
}

fn find(host: &Host, name: &str, input: bool) -> Option<Device> {
    if name.is_empty() {
        return if input { host.default_input_device() } else { host.default_output_device() };
    }
    let devs: Vec<Device> = if input { host.input_devices().ok()?.collect() } else { host.output_devices().ok()?.collect() };
    devs.into_iter().find(|d| d.name().ok().as_deref() == Some(name))
}

fn open(dev: &Device, cfg: SupportedStreamConfig) -> Result<(Stream, Queue), String> {
    if cfg.sample_format() != SampleFormat::F32 {
        return Err(format!("unsupported sample format {:?}", cfg.sample_format()));
    }
    let ch = cfg.channels() as usize;
    let mut rs = Resampler::new(cfg.sample_rate().0);
    let q: Queue = Arc::new(Mutex::new(VecDeque::with_capacity(RATE as usize)));
    let q2 = q.clone();
    let stream = dev
        .build_input_stream(
            &cfg.config(),
            move |data: &[f32], _| rs.process(data, ch, &mut q2.lock()),
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, q))
}

fn src_mic(host: &Host, name: &str, ch: Arc<Chan>) -> Result<Src, String> {
    let dev = find(host, name, true).ok_or_else(|| "microphone not found".to_string())?;
    let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
    let (s, q) = open(&dev, cfg)?;
    Ok(Src { q, ch, _stream: Some(s), stop: None })
}

fn src_desk(host: &Host, name: &str, ch: Arc<Chan>) -> Result<Src, String> {
    // On Windows an input stream built on an *output* device is WASAPI loopback.
    let dev = find(host, name, false).ok_or_else(|| "output device not found".to_string())?;
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    let (s, q) = open(&dev, cfg)?;
    Ok(Src { q, ch, _stream: Some(s), stop: None })
}

#[cfg(feature = "app-audio")]
fn src_app(pid: u32, ch: Arc<Chan>, err: Arc<Mutex<Option<String>>>) -> Result<Src, String> {
    let q: Queue = Arc::new(Mutex::new(VecDeque::with_capacity(RATE as usize)));
    let stop = Arc::new(AtomicBool::new(false));
    let (q2, s2) = (q.clone(), stop.clone());
    thread::spawn(move || {
        if let Err(e) = app_capture(pid, &q2, &s2) {
            *err.lock() = Some(format!("App audio: {e}"));
        }
    });
    Ok(Src { q, ch, _stream: None, stop: Some(stop) })
}

#[cfg(not(feature = "app-audio"))]
fn src_app(_pid: u32, _ch: Arc<Chan>, _err: Arc<Mutex<Option<String>>>) -> Result<Src, String> {
    Err("this build has no per-app audio support".into())
}

/// Windows 10 build 19041+ "process loopback": captures only one app (and its child processes).
#[cfg(feature = "app-audio")]
fn app_capture(pid: u32, q: &Queue, stop: &AtomicBool) -> Result<(), String> {
    use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};
    let _ = wasapi::initialize_mta();
    let mut client = AudioClient::new_application_loopback_client(pid, true).map_err(|e| format!("{e}"))?;
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, RATE as usize, 2, None);
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    client.initialize_client(&fmt, &Direction::Capture, &mode).map_err(|e| format!("{e}"))?;
    let ev = client.set_get_eventhandle().map_err(|e| format!("{e}"))?;
    let cap = client.get_audiocaptureclient().map_err(|e| format!("{e}"))?;
    client.start_stream().map_err(|e| format!("{e}"))?;
    let mut raw: VecDeque<u8> = VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        if ev.wait_for_event(200).is_err() { continue; } // no audio playing right now
        cap.read_from_device_to_deque(&mut raw).map_err(|e| format!("{e}"))?;
        if raw.len() >= 4 {
            let mut qq = q.lock();
            while raw.len() >= 4 {
                let b = [raw.pop_front().unwrap(), raw.pop_front().unwrap(), raw.pop_front().unwrap(), raw.pop_front().unwrap()];
                qq.push_back(f32::from_le_bytes(b));
            }
            cap_q(&mut qq);
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

#[cfg(feature = "app-audio")]
fn cap_q(q: &mut VecDeque<f32>) { cap(q) }

fn engine_loop(rx: Receiver<Cmd>, chans: [Arc<Chan>; 3], err: Arc<Mutex<Option<String>>>) {
    let host = cpal::default_host();
    let mut srcs: Vec<Src> = Vec::new();
    let mut sink: Option<TcpStream> = None;
    let mut clock = Instant::now();
    let mut sent: u64 = 0;
    let mut mix: Vec<f32> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();

    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Setup(s)) => {
                    srcs.clear();
                    *err.lock() = None;
                    for c in &chans { c.set_peak(0.0); }
                    let note = |label: &str, r: Result<Src, String>, srcs: &mut Vec<Src>| match r {
                        Ok(s) => srcs.push(s),
                        Err(e) => *err.lock() = Some(format!("{label}: {e}")),
                    };
                    if let Some(n) = &s.mic { note("Mic", src_mic(&host, n, chans[0].clone()), &mut srcs); }
                    if let Some(n) = &s.desk { note("Desktop audio", src_desk(&host, n, chans[1].clone()), &mut srcs); }
                    if let Some(p) = s.app { note("App audio", src_app(p, chans[2].clone(), err.clone()), &mut srcs); }
                }
                Ok(Cmd::Attach(s)) => { sink = Some(s); clock = Instant::now(); sent = 0; }
                Ok(Cmd::Detach) => { sink = None; }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }

        let due = (clock.elapsed().as_secs_f64() * RATE as f64) as u64;
        if due > sent {
            let n = ((due - sent) as usize).min(RATE as usize);
            mix.clear();
            mix.resize(n * 2, 0.0);
            for s in &srcs {
                let g = if s.ch.muted() { 0.0 } else { s.ch.gain() };
                let mut q = s.q.lock();
                let mut peak = 0f32;
                for m in mix.iter_mut() {
                    match q.pop_front() {
                        Some(v) => { let v = v * g; peak = peak.max(v.abs()); *m += v; }
                        None => break,
                    }
                }
                let keep = 4800 * 2; // bound latency if a device clock runs fast
                if q.len() > keep * 2 { let d = q.len() - keep; q.drain(..d); }
                s.ch.set_peak(peak.max(s.ch.peak() * 0.9));
            }
            if let Some(sock) = sink.as_mut() {
                bytes.clear();
                for v in &mix {
                    bytes.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                if sock.write_all(&bytes).is_err() { sink = None; }
            }
            sent += n as u64;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

//! capture session -> fixed-FPS pacer -> ffmpeg (libx264); audio arrives from the audio engine over TCP.
use crate::audio::Cmd;
use crate::capture;
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct RecConfig {
    pub shared: Arc<capture::Shared>, // from the running capture session
    pub out_dir: String,
    pub mkv: bool,
    pub fps: u32,
    pub out_height: Option<u32>, // None = native
    pub preset: String,
    pub crf: u32,
    pub threads: u32,
    pub audio_tx: Option<Sender<Cmd>>, // Some = record audio
}

pub struct Recorder {
    stop: Arc<AtomicBool>,
    shared: Arc<capture::Shared>,
    err: Arc<Mutex<Option<String>>>,
    frames: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    started: Instant,
    pub path: PathBuf,
    handles: Vec<JoinHandle<()>>,
    child: Child,
    audio_tx: Option<Sender<Cmd>>,
}

pub struct Stats {
    pub elapsed: Duration,
    pub frames: u64,
    pub dropped: u64,
    pub size: u64,
    pub err: Option<String>,
}

pub fn ffmpeg_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("ffmpeg.exe");
            if p.exists() { return p; }
        }
    }
    PathBuf::from("ffmpeg")
}

impl Recorder {
    pub fn start(cfg: &RecConfig) -> Result<Self> {
        let shared = cfg.shared.clone();
        let t0 = Instant::now();
        let (w, h) = loop {
            {
                let f = shared.frame.lock();
                if f.w > 0 && !f.data.is_empty() { break (f.w, f.h); }
            }
            if let Some(e) = shared.err.lock().clone() { bail!(e); }
            if t0.elapsed() > Duration::from_secs(5) { bail!("No frames received from the capture source"); }
            thread::sleep(Duration::from_millis(20));
        };

        let (ow, oh) = match cfg.out_height {
            Some(t) if t < h => ((((w as u64 * t as u64) / h as u64) as u32) & !1, t & !1),
            _ => (w & !1, h & !1),
        };

        std::fs::create_dir_all(&cfg.out_dir).ok();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = PathBuf::from(&cfg.out_dir).join(format!("literec_{secs}.{}", if cfg.mkv { "mkv" } else { "mp4" }));

        let listener = if cfg.audio_tx.is_some() { Some(TcpListener::bind("127.0.0.1:0")?) } else { None };

        let mut a: Vec<String> = vec![
            "-hide_banner".into(), "-loglevel".into(), "warning".into(), "-y".into(),
            // wall-clock timestamps + cfr output: if the PC falls behind, ffmpeg keeps A/V in sync
            "-use_wallclock_as_timestamps".into(), "1".into(),
            "-f".into(), "rawvideo".into(), "-pix_fmt".into(), "bgra".into(),
            "-video_size".into(), format!("{w}x{h}"),
            "-framerate".into(), cfg.fps.to_string(),
            "-i".into(), "-".into(),
        ];
        if let Some(l) = &listener {
            let port = l.local_addr()?.port();
            a.extend([
                "-f".into(), "s16le".into(), "-ar".into(), "48000".into(), "-ac".into(), "2".into(),
                "-i".into(), format!("tcp://127.0.0.1:{port}"),
            ]);
        }
        a.extend([
            "-vf".into(), format!("scale={ow}:{oh}:flags=fast_bilinear,format=yuv420p"),
            "-fps_mode".into(), "cfr".into(), "-r".into(), cfg.fps.to_string(),
            "-c:v".into(), "libx264".into(),
            "-preset".into(), cfg.preset.clone(),
            "-crf".into(), cfg.crf.to_string(),
            "-threads".into(), cfg.threads.to_string(),
            "-g".into(), (cfg.fps * 2).to_string(),
        ]);
        if listener.is_some() { a.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "160k".into()]); }
        if !cfg.mkv { a.extend(["-movflags".into(), "+faststart".into()]); }
        a.push(path.to_string_lossy().into_owned());

        let log = std::fs::File::create(path.with_extension("log"))?;
        let mut cmd = Command::new(ffmpeg_path());
        cmd.args(&a).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(log);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().context("Could not start ffmpeg.exe (put it next to literec.exe or on PATH)")?;
        let mut stdin = child.stdin.take().context("ffmpeg stdin")?;

        let stop = Arc::new(AtomicBool::new(false));
        let err = Arc::new(Mutex::new(None));
        let frames = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();

        if let (Some(l), Some(tx)) = (listener, cfg.audio_tx.clone()) {
            let (stop, err) = (stop.clone(), err.clone());
            handles.push(thread::spawn(move || {
                l.set_nonblocking(true).ok();
                let t0 = Instant::now();
                loop {
                    match l.accept() {
                        Ok((s, _)) => {
                            s.set_nonblocking(false).ok();
                            s.set_nodelay(true).ok();
                            let _ = tx.send(Cmd::Attach(s));
                            return;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::Relaxed) { return; }
                            if t0.elapsed() > Duration::from_secs(10) {
                                *err.lock() = Some("ffmpeg never connected to the audio socket".into());
                                return;
                            }
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => return,
                    }
                }
            }));
        }

        {
            let (stop, shared, err, frames, dropped) = (stop.clone(), shared.clone(), err.clone(), frames.clone(), dropped.clone());
            let fps = cfg.fps;
            handles.push(thread::spawn(move || {
                let period = Duration::from_secs_f64(1.0 / fps as f64);
                let expect = (w as usize) * (h as usize) * 4;
                let mut buf: Vec<u8> = Vec::with_capacity(expect);
                let mut next = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    {
                        let f = shared.frame.lock();
                        if f.w == w && f.h == h && f.data.len() == expect {
                            buf.clear();
                            buf.extend_from_slice(&f.data);
                        }
                    } // if the source was resized, the last good frame is repeated
                    if buf.len() == expect {
                        if stdin.write_all(&buf).is_err() {
                            *err.lock() = Some("ffmpeg stopped unexpectedly (see the .log next to the video)".into());
                            break;
                        }
                        frames.fetch_add(1, Ordering::Relaxed);
                    }
                    next += period;
                    let now = Instant::now();
                    if next > now {
                        thread::sleep(next - now);
                    } else {
                        // we were too slow (ffmpeg backed up): count the missed ticks as dropped
                        let late = ((now - next).as_nanos() / period.as_nanos().max(1)) as u64;
                        if late > 0 {
                            dropped.fetch_add(late, Ordering::Relaxed);
                            if late > 1000 { next = now; } else { next += period * late as u32; }
                        }
                    }
                }
            }));
        }

        Ok(Self { stop, shared, err, frames, dropped, started: Instant::now(), path, handles, child, audio_tx: cfg.audio_tx.clone() })
    }

    pub fn stats(&self) -> Stats {
        Stats {
            elapsed: self.started.elapsed(),
            frames: self.frames.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            size: std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
            err: self.err.lock().clone().or_else(|| self.shared.err.lock().clone()),
        }
    }

    /// Stops asynchronously so the UI never freezes; `done` flips when the file is finalized.
    /// (The capture session is owned by the app and keeps running.)
    pub fn stop(self, done: Arc<AtomicBool>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(tx) = &self.audio_tx { let _ = tx.send(Cmd::Detach); }
        thread::spawn(move || {
            let mut s = self;
            for h in s.handles.drain(..) { let _ = h.join(); }
            let t0 = Instant::now();
            loop {
                match s.child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => {
                        if t0.elapsed() > Duration::from_secs(20) { let _ = s.child.kill(); break; }
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            done.store(true, Ordering::Relaxed);
        });
    }
}

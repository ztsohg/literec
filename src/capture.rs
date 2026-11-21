//! Screen/window capture. Two back-ends:
//!  * DXGI Desktop Duplication (displays only, NO yellow border, works on Win8+/Win10)
//!  * Windows Graphics Capture (displays + single windows; yellow border on older Win10)
//! A capture `Session` only keeps the *latest* frame; the recorder paces it to a fixed FPS.
use crate::cursor;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{ColorFormat, CursorCaptureSettings, DrawBorderSettings, Settings},
    window::Window,
};

#[derive(Clone, PartialEq)]
pub enum Target {
    /// index (0-based), `dxgi` = true -> Desktop Duplication (no border)
    Display { idx: usize, dxgi: bool, cursor: bool },
    /// window title (substring match), always Graphics Capture
    Window { title: String, cursor: bool },
}

#[derive(Default)]
pub struct FrameBuf {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>, // tightly packed BGRA
}

#[derive(Default)]
pub struct Shared {
    pub frame: Mutex<FrameBuf>,
    pub stop: AtomicBool,
    pub err: Mutex<Option<String>>,
    /// max capture rate; the app lowers it when only previewing (saves CPU on old PCs)
    pub fps: AtomicU32,
}

pub struct Session {
    pub shared: Arc<Shared>,
    pub target: Target,
}

impl Session {
    pub fn start(target: Target) -> Self {
        let shared = Arc::new(Shared::default());
        shared.fps.store(10, Ordering::Relaxed);
        let (t, s) = (target.clone(), shared.clone());
        std::thread::spawn(move || {
            let r = match t {
                Target::Display { idx, dxgi: true, cursor } => run_dxgi(idx, cursor, &s),
                Target::Display { idx, dxgi: false, cursor } => run_wgc_display(idx, cursor, &s),
                Target::Window { title, cursor } => run_wgc_window(&title, cursor, &s),
            };
            if let Err(e) = r {
                *s.err.lock() = Some(format!("Capture failed: {e}"));
            }
        });
        Self { shared, target }
    }
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

fn store(shared: &Shared, w: u32, h: u32, data: &[u8]) {
    let mut f = shared.frame.lock();
    f.w = w;
    f.h = h;
    f.data.clear();
    f.data.extend_from_slice(data);
}

// ---------- DXGI Desktop Duplication (no border) ----------
// Desktop Duplication never contains the mouse pointer, so when `cursor` is on we keep the last
// clean frame and draw the pointer onto a copy every cycle (this also keeps the pointer moving
// smoothly on an otherwise static screen).
fn run_dxgi(idx: usize, cursor: bool, shared: &Arc<Shared>) -> Result<(), String> {
    use dxgi_capture_rs::DXGIManager;
    let make = || -> Result<DXGIManager, String> {
        let mut m = DXGIManager::new(33).map_err(|e| format!("{e:?}"))?;
        m.set_capture_source_index(idx);
        Ok(m)
    };
    let mut mgr = make()?;
    let mut overlay = cursor::Overlay::default();
    let mut clean: Vec<u8> = Vec::new();
    let (mut cw, mut ch) = (0usize, 0usize);
    let mut origin: Option<((usize, usize), (i32, i32))> = None;

    while !shared.stop.load(Ordering::Relaxed) {
        let fps = shared.fps.load(Ordering::Relaxed).max(1);
        let period = Duration::from_secs_f64(1.0 / fps as f64);
        let t_start = Instant::now();
        mgr.set_timeout_ms((period.as_millis() as u32).max(1));

        let mut changed = false;
        match mgr.capture_frame_components() {
            Ok((bytes, (w, h))) => {
                clean = bytes;
                cw = w;
                ch = h;
                changed = true;
            }
            Err(e) => {
                let s = format!("{e:?}");
                if s.contains("Timeout") {
                    // nothing new on screen
                } else if s.contains("AccessLost") || s.contains("RefreshFailure") {
                    std::thread::sleep(Duration::from_millis(300));
                    mgr = make()?; // resolution change, UAC prompt, etc.
                } else {
                    return Err(s);
                }
            }
        }

        if cw > 0 && clean.len() == cw * ch * 4 && (changed || cursor) {
            let mut f = shared.frame.lock();
            f.w = cw as u32;
            f.h = ch as u32;
            f.data.clear();
            f.data.extend_from_slice(&clean);
            if cursor {
                let o = match origin {
                    Some((sz, o)) if sz == (cw, ch) => o,
                    _ => {
                        let o = cursor::origin_for(idx, cw, ch);
                        origin = Some(((cw, ch), o));
                        o
                    }
                };
                overlay.draw(&mut f.data, cw, ch, o);
            }
        }

        let el = t_start.elapsed();
        if el < period {
            std::thread::sleep(period - el);
        }
    }
    Ok(())
}

// ---------- Windows Graphics Capture ----------
struct Grabber {
    shared: Arc<Shared>,
    last: Instant,
}

impl GraphicsCaptureApiHandler for Grabber {
    type Flags = Arc<Shared>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let now = Instant::now();
        Ok(Self { shared: ctx.flags, last: now.checked_sub(Duration::from_secs(1)).unwrap_or(now) })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if self.shared.stop.load(Ordering::Relaxed) {
            control.stop();
            return Ok(());
        }
        let fps = self.shared.fps.load(Ordering::Relaxed).max(1);
        if self.last.elapsed() < Duration::from_secs_f64(0.9 / fps as f64) {
            return Ok(()); // above the requested rate: skip the copy
        }
        self.last = Instant::now();
        let (w, h) = (frame.width(), frame.height());
        let mut buf = frame.buffer()?;
        let data = buf.as_nopadding_buffer()?;
        store(&self.shared, w, h, data);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn cursor_setting(on: bool) -> CursorCaptureSettings {
    if on { CursorCaptureSettings::WithCursor } else { CursorCaptureSettings::WithoutCursor }
}

fn run_wgc_display(idx: usize, cursor: bool, shared: &Arc<Shared>) -> Result<(), String> {
    let m = Monitor::from_index(idx + 1).map_err(|e| e.to_string())?;
    let s = Settings::new(
        m,
        cursor_setting(cursor),
        DrawBorderSettings::Default,
        ColorFormat::Bgra8,
        shared.clone(),
    );
    Grabber::start(s).map_err(|e| e.to_string())
}

fn run_wgc_window(title: &str, cursor: bool, shared: &Arc<Shared>) -> Result<(), String> {
    let w = Window::from_name(title).map_err(|e| e.to_string())?;
    let s = Settings::new(
        w,
        cursor_setting(cursor),
        DrawBorderSettings::Default,
        ColorFormat::Bgra8,
        shared.clone(),
    );
    Grabber::start(s).map_err(|e| e.to_string())
}

pub fn list_displays() -> Vec<String> {
    Monitor::enumerate()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            format!("Display {} ({}x{})", i + 1, m.width().unwrap_or(0), m.height().unwrap_or(0))
        })
        .collect()
}

pub fn list_windows() -> Vec<String> {
    let mut v: Vec<String> = Window::enumerate()
        .unwrap_or_default()
        .iter()
        .filter_map(|w| w.title().ok())
        .filter(|t| !t.trim().is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod audio;
mod capture;
mod cursor;
mod recorder;
mod sys;
mod win;

use audio::{Chan, Engine, Setup};
use capture::{Session, Target};
use eframe::egui;
use egui::{Align, Color32, Layout, RichText};
use recorder::{RecConfig, Recorder};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const LOGO: &[u8] = include_bytes!("../assets/logo.png");
const ACCENT: Color32 = Color32::from_rgb(235, 45, 45);

const RES: [(&str, u32); 7] = [
    ("Native", 0), ("1080p", 1080), ("900p", 900), ("720p", 720), ("576p", 576), ("480p", 480), ("360p", 360),
];
const PRESETS: [&str; 3] = ["ultrafast", "superfast", "veryfast"];
const FPS: [u32; 4] = [15, 24, 30, 60];
const PREV_W: [(&str, usize); 4] = [("Low (320 px)", 320), ("Medium (480 px)", 480), ("High (640 px)", 640), ("Ultra (960 px)", 960)];
const PREV_FPS: [u32; 4] = [5, 10, 15, 30];
const TABS: [&str; 5] = ["General", "Output", "Video", "Audio", "About"];

struct Preset { name: &'static str, res: usize, fps: usize, preset: usize, crf: u32, threads: u32 }
const PRESET_TABLE: [Preset; 4] = [
    Preset { name: "Potato (480p 24fps)", res: 5, fps: 1, preset: 0, crf: 30, threads: 2 },
    Preset { name: "Balanced (720p 30fps)", res: 3, fps: 2, preset: 0, crf: 26, threads: 3 },
    Preset { name: "Smooth (720p 60fps)", res: 3, fps: 3, preset: 0, crf: 28, threads: 4 },
    Preset { name: "Quality (1080p 30fps)", res: 1, fps: 2, preset: 1, crf: 24, threads: 4 },
];

struct App {
    // sources
    displays: Vec<String>,
    windows: Vec<String>,
    kind: usize, // 0 display, 1 window
    disp_idx: usize,
    win_idx: usize,
    use_dxgi: bool,
    use_cursor: bool,
    // video
    res_idx: usize,
    fps_idx: usize,
    preset_idx: usize,
    crf: u32,
    threads: u32,
    mkv: bool,
    out_dir: String,
    // audio
    use_mic: bool,
    use_desk: bool,
    use_app: bool,
    mic_dev: String,
    desk_dev: String,
    in_devs: Vec<String>,
    out_devs: Vec<String>,
    apps: Vec<sys::AppEntry>,
    app_sel: Option<usize>,
    mic: Arc<Chan>,
    desk: Arc<Chan>,
    app: Arc<Chan>,
    engine: Engine,
    last_setup: Option<Setup>,
    // capture / preview
    session: Option<Session>,
    preview: bool,
    prev_q: usize,
    prev_fps: usize,
    hide_self: bool,
    hide_applied: Option<bool>,
    frame_no: u64,
    dump_after: Option<u64>,
    preview_tex: Option<egui::TextureHandle>,
    last_preview: Instant,
    // ui
    show_settings: bool,
    settings_tab: usize,
    logo_tex: Option<egui::TextureHandle>,
    // tray
    tray_mode: bool,
    tray: Option<TrayIcon>,
    tray_rec: Arc<AtomicBool>,
    rec_item: Option<MenuItem>,
    rec_item_text: String,
    quitting: Arc<AtomicBool>,
    quit_seen: Arc<AtomicBool>,
    icon_small: Vec<u8>, // 32x32 RGBA
    // state
    rec: Option<Recorder>,
    finalizing: Option<(Arc<AtomicBool>, String)>,
    status: String,
    rec_secs: u64,
    rec_size: u64,
    cpu: sys::CpuMeter,
    last_frames: u64,
    last_dropped: u64,
}

fn settings_path() -> PathBuf {
    let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    base.join("LiteRec").join("settings.ini")
}

fn p<T: std::str::FromStr>(m: &HashMap<&str, &str>, k: &str, v: &mut T) {
    if let Some(x) = m.get(k).and_then(|s| s.trim().parse().ok()) { *v = x; }
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}...", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}

// ---------------- theme & small widgets ----------------
fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(28, 28, 32);
    v.window_fill = Color32::from_rgb(34, 34, 40);
    v.extreme_bg_color = Color32::from_rgb(16, 16, 18);
    v.faint_bg_color = Color32::from_rgb(36, 36, 41);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = ACCENT;
    let idle = Color32::from_rgb(52, 52, 60);
    let hover = Color32::from_rgb(70, 70, 80);
    let press = Color32::from_rgb(190, 35, 35);
    v.widgets.inactive.bg_fill = idle;
    v.widgets.inactive.weak_bg_fill = idle;
    v.widgets.hovered.bg_fill = hover;
    v.widgets.hovered.weak_bg_fill = hover;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    v.widgets.active.bg_fill = press;
    v.widgets.active.weak_bg_fill = press;
    let r = egui::Rounding::same(3.0);
    v.widgets.noninteractive.rounding = r;
    v.widgets.inactive.rounding = r;
    v.widgets.hovered.rounding = r;
    v.widgets.active.rounding = r;
    v.widgets.open.rounding = r;
    ctx.set_visuals(v);
}

/// OBS-style dock: titled box with a separator under the title.
fn dock(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .rounding(3.0)
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong());
            ui.separator();
            add(ui);
        });
}

/// Level meter: -60 dB .. 0 dB, green / yellow / red like OBS.
fn meter(ui: &mut egui::Ui, peak: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_rgb(14, 14, 16));
    let db = if peak > 0.00001 { 20.0 * peak.log10() } else { -60.0 };
    let fill = rect.width() * ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let seg = |a: f32, b: f32, c: Color32| {
        let x0 = rect.left() + rect.width() * a;
        let x1 = rect.left() + (rect.width() * b).min(fill);
        if x1 > x0 {
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())), 0.0, c);
        }
    };
    seg(0.0, 0.7, Color32::from_rgb(70, 190, 90));
    seg(0.7, 0.9, Color32::from_rgb(230, 200, 60));
    seg(0.9, 1.0, Color32::from_rgb(235, 60, 60));
}

fn mixer_channel(ui: &mut egui::Ui, name: &str, sub: &str, enabled: &mut bool, locked: bool, ch: &Chan) {
    ui.horizontal(|ui| {
        ui.add_enabled(!locked, egui::Checkbox::new(enabled, RichText::new(name).strong()));
        ui.label(RichText::new(sub).small().weak());
    });
    meter(ui, ch.peak());
    ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 125.0).max(50.0);
        let mut g = ch.gain();
        if ui.add(egui::Slider::new(&mut g, 0.0..=2.0).show_value(false)).changed() { ch.set_gain(g); }
        let mut pct = (ch.gain() * 100.0).round();
        if ui.add(egui::DragValue::new(&mut pct).range(0.0..=200.0).speed(1.0).suffix("%")).changed() {
            ch.set_gain(pct / 100.0);
        }
        let mut m = ch.muted();
        if ui.toggle_value(&mut m, "Mute").changed() { ch.set_muted(m); }
    });
    ui.add_space(4.0);
}

fn dev_combo(ui: &mut egui::Ui, id: &str, sel: &mut String, list: &[String]) {
    let shown = if sel.is_empty() { "Default".to_string() } else { short(sel, 40) };
    egui::ComboBox::from_id_salt(id).width(300.0).selected_text(shown).show_ui(ui, |ui| {
        ui.selectable_value(sel, String::new(), "Default");
        for n in list { ui.selectable_value(sel, n.clone(), short(n, 50)); }
    });
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        apply_theme(ctx);
        let out = std::env::var("USERPROFILE").map(|p| format!("{p}\\Videos")).unwrap_or_else(|_| ".".into());
        let (in_devs, out_devs) = audio::list_devices();
        let mic = Arc::new(Chan::new(1.0));
        let desk = Arc::new(Chan::new(1.0));
        let app = Arc::new(Chan::new(1.0));
        let engine = Engine::new(mic.clone(), desk.clone(), app.clone());
        let logo = image::load_from_memory(LOGO).ok().map(|i| i.to_rgba8());
        let icon_small = logo
            .as_ref()
            .map(|i| image::imageops::resize(i, 32, 32, image::imageops::FilterType::Triangle).into_raw())
            .unwrap_or_default();
        let logo_tex = logo.map(|i| {
            let img = egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], i.as_raw());
            ctx.load_texture("logo", img, egui::TextureOptions::LINEAR)
        });
        let mut a = Self {
            displays: capture::list_displays(),
            windows: capture::list_windows(),
            kind: 0, disp_idx: 0, win_idx: 0, use_dxgi: true, use_cursor: true,
            res_idx: 3, fps_idx: 2, preset_idx: 0, crf: 26, threads: 3, mkv: true,
            out_dir: out,
            use_mic: true, use_desk: true, use_app: false,
            mic_dev: String::new(), desk_dev: String::new(),
            in_devs, out_devs,
            apps: sys::list_apps(), app_sel: None,
            mic, desk, app, engine, last_setup: None,
            session: None, preview: true, prev_q: 2, prev_fps: 1, hide_self: true, hide_applied: None, frame_no: 0, dump_after: None, preview_tex: None, last_preview: Instant::now(),
            show_settings: false, settings_tab: 0, logo_tex,
            tray_mode: false, tray: None,
            tray_rec: Arc::new(AtomicBool::new(false)),
            rec_item: None,
            rec_item_text: String::new(),
            quitting: Arc::new(AtomicBool::new(false)),
            quit_seen: Arc::new(AtomicBool::new(false)),
            icon_small,
            rec: None, finalizing: None, status: "Ready".into(),
            rec_secs: 0, rec_size: 0,
            cpu: sys::CpuMeter::new(), last_frames: 0, last_dropped: 0,
        };
        a.load();
        a
    }

    // ---------- settings file ----------
    fn load(&mut self) {
        let Ok(txt) = std::fs::read_to_string(settings_path()) else { return };
        let m: HashMap<&str, &str> = txt.lines().filter_map(|l| l.split_once('=')).collect();
        p(&m, "kind", &mut self.kind);
        p(&m, "disp_idx", &mut self.disp_idx);
        p(&m, "use_dxgi", &mut self.use_dxgi);
        p(&m, "cursor", &mut self.use_cursor);
        p(&m, "res_idx", &mut self.res_idx);
        p(&m, "fps_idx", &mut self.fps_idx);
        p(&m, "preset_idx", &mut self.preset_idx);
        p(&m, "crf", &mut self.crf);
        p(&m, "threads", &mut self.threads);
        p(&m, "mkv", &mut self.mkv);
        p(&m, "use_mic", &mut self.use_mic);
        p(&m, "use_desk", &mut self.use_desk);
        p(&m, "use_app", &mut self.use_app);
        p(&m, "preview", &mut self.preview);
        p(&m, "prev_q", &mut self.prev_q);
        p(&m, "prev_fps", &mut self.prev_fps);
        p(&m, "hide_self", &mut self.hide_self);
        p(&m, "tray_mode", &mut self.tray_mode);
        let mut g = 1.0f32;
        p(&m, "mic_gain", &mut g); self.mic.set_gain(g);
        g = 1.0; p(&m, "desk_gain", &mut g); self.desk.set_gain(g);
        g = 1.0; p(&m, "app_gain", &mut g); self.app.set_gain(g);
        if let Some(v) = m.get("out_dir") { if !v.trim().is_empty() { self.out_dir = v.to_string(); } }
        if let Some(v) = m.get("mic_dev") { self.mic_dev = v.to_string(); }
        if let Some(v) = m.get("desk_dev") { self.desk_dev = v.to_string(); }
        if let Some(t) = m.get("win_title") { if let Some(i) = self.windows.iter().position(|w| w == t) { self.win_idx = i; } }
        if let Some(e) = m.get("app_exe") { self.app_sel = self.apps.iter().position(|a| a.exe == *e); }
        self.disp_idx = self.disp_idx.min(self.displays.len().saturating_sub(1));
        self.res_idx = self.res_idx.min(RES.len() - 1);
        self.fps_idx = self.fps_idx.min(FPS.len() - 1);
        self.preset_idx = self.preset_idx.min(PRESETS.len() - 1);
        self.kind = self.kind.min(1);
        self.prev_q = self.prev_q.min(PREV_W.len() - 1);
        self.prev_fps = self.prev_fps.min(PREV_FPS.len() - 1);
    }

    fn save_settings(&self) {
        let path = settings_path();
        if let Some(d) = path.parent() { let _ = std::fs::create_dir_all(d); }
        let s = format!(
            "kind={}\ndisp_idx={}\nuse_dxgi={}\ncursor={}\nwin_title={}\nres_idx={}\nfps_idx={}\npreset_idx={}\ncrf={}\nthreads={}\nmkv={}\nout_dir={}\n\
             use_mic={}\nuse_desk={}\nuse_app={}\nmic_dev={}\ndesk_dev={}\napp_exe={}\nmic_gain={}\ndesk_gain={}\napp_gain={}\npreview={}\nprev_q={}\nprev_fps={}\nhide_self={}\ntray_mode={}\n",
            self.kind, self.disp_idx, self.use_dxgi, self.use_cursor,
            self.windows.get(self.win_idx).cloned().unwrap_or_default(),
            self.res_idx, self.fps_idx, self.preset_idx, self.crf, self.threads, self.mkv, self.out_dir,
            self.use_mic, self.use_desk, self.use_app, self.mic_dev, self.desk_dev,
            self.app_sel.and_then(|i| self.apps.get(i)).map(|a| a.exe.clone()).unwrap_or_default(),
            self.mic.gain(), self.desk.gain(), self.app.gain(), self.preview, self.prev_q, self.prev_fps, self.hide_self, self.tray_mode,
        );
        let _ = std::fs::write(path, s);
    }

    // ---------- presets ----------
    fn current_preset(&self) -> Option<usize> {
        PRESET_TABLE.iter().position(|p| {
            p.res == self.res_idx && p.fps == self.fps_idx && p.preset == self.preset_idx && p.crf == self.crf && p.threads == self.threads
        })
    }
    fn apply_preset(&mut self, i: usize) {
        let p = &PRESET_TABLE[i];
        self.res_idx = p.res; self.fps_idx = p.fps; self.preset_idx = p.preset; self.crf = p.crf; self.threads = p.threads;
    }
    fn preset_combo(&mut self, ui: &mut egui::Ui, id: &str, width: f32) {
        let name = self.current_preset().map(|i| PRESET_TABLE[i].name).unwrap_or("Custom");
        egui::ComboBox::from_id_salt(id).width(width).selected_text(name).show_ui(ui, |ui| {
            for (i, pr) in PRESET_TABLE.iter().enumerate() {
                if ui.selectable_label(self.current_preset() == Some(i), pr.name).clicked() { self.apply_preset(i); }
            }
        });
    }

    // ---------- capture session / preview ----------
    fn target(&self) -> Option<Target> {
        if self.kind == 0 {
            Some(Target::Display { idx: self.disp_idx, dxgi: self.use_dxgi, cursor: self.use_cursor })
        } else {
            self.windows.get(self.win_idx).cloned().map(|title| Target::Window { title, cursor: self.use_cursor })
        }
    }

    fn sync_session(&mut self, force: bool) {
        let need = force || (self.preview && !win::is_hidden()) || self.rec.is_some() || self.finalizing.is_some();
        let want = if need { self.target() } else { None };
        let same = matches!((&self.session, &want), (Some(s), Some(w)) if s.target == *w);
        if !same {
            if let Some(s) = self.session.take() { s.stop(); }
            if let Some(w) = want { self.session = Some(Session::start(w)); }
        }
        if let Some(s) = &self.session {
            let f = if self.rec.is_some() { FPS[self.fps_idx] } else { PREV_FPS[self.prev_fps] };
            s.shared.fps.store(f, Ordering::Relaxed);
        }
    }

    fn update_preview(&mut self, ctx: &egui::Context) {
        if !self.preview { self.preview_tex = None; return; }
        if win::is_hidden() { return; }
        if self.last_preview.elapsed() < Duration::from_millis(1000 / PREV_FPS[self.prev_fps] as u64) { return; }
        let Some(s) = &self.session else { return };
        self.last_preview = Instant::now();
        let f = s.shared.frame.lock();
        let (w, h) = (f.w as usize, f.h as usize);
        if w == 0 || f.data.len() != w * h * 4 { return; }
        let pw = PREV_W[self.prev_q].1.min(w);
        let ph = (h * pw / w).max(1);
        let mut rgba = vec![255u8; pw * ph * 4];
        for y in 0..ph {
            let sy = y * h / ph;
            for x in 0..pw {
                let i = (sy * w + x * w / pw) * 4;
                let o = (y * pw + x) * 4;
                rgba[o] = f.data[i + 2];
                rgba[o + 1] = f.data[i + 1];
                rgba[o + 2] = f.data[i];
            }
        }
        drop(f);
        let img = egui::ColorImage::from_rgba_unmultiplied([pw, ph], &rgba);
        match &mut self.preview_tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.preview_tex = Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR)),
        }
    }

    // ---------- audio ----------
    fn audio_setup(&self) -> Setup {
        Setup {
            mic: self.use_mic.then(|| self.mic_dev.clone()),
            desk: self.use_desk.then(|| self.desk_dev.clone()),
            app: if self.use_app { self.app_sel.and_then(|i| self.apps.get(i)).map(|a| a.pid) } else { None },
        }
    }

    fn refresh_lists(&mut self) {
        self.displays = capture::list_displays();
        self.windows = capture::list_windows();
        let (i, o) = audio::list_devices();
        self.in_devs = i;
        self.out_devs = o;
        let exe = self.app_sel.and_then(|i| self.apps.get(i)).map(|a| a.exe.clone());
        self.apps = sys::list_apps();
        self.app_sel = exe.and_then(|e| self.apps.iter().position(|a| a.exe == e));
        self.last_setup = None;
    }

    // ---------- recording ----------
    fn start(&mut self) {
        if self.target().is_none() { self.status = "Error: no window selected".into(); return; }
        self.sync_session(true);
        let Some(session) = &self.session else { return };
        let setup = self.audio_setup();
        let h = RES[self.res_idx].1;
        let cfg = RecConfig {
            shared: session.shared.clone(),
            out_dir: self.out_dir.clone(),
            mkv: self.mkv,
            fps: FPS[self.fps_idx],
            out_height: if h == 0 { None } else { Some(h) },
            preset: PRESETS[self.preset_idx].into(),
            crf: self.crf,
            threads: self.threads,
            audio_tx: setup.any().then(|| self.engine.sender()),
        };
        self.status = "Starting...".into();
        if let Some(s) = &self.session { s.shared.fps.store(cfg.fps, Ordering::Relaxed); }
        match Recorder::start(&cfg) {
            Ok(r) => { self.status = format!("Recording to {}", r.path.display()); self.rec = Some(r); self.save_settings(); }
            Err(e) => { self.status = format!("Error: {e:#}"); self.sync_session(false); }
        }
    }

    fn stop(&mut self) {
        if let Some(r) = self.rec.take() {
            let done = Arc::new(AtomicBool::new(false));
            let path = r.path.display().to_string();
            r.stop(done.clone());
            self.status = "Finalizing file...".into();
            self.finalizing = Some((done, path));
        }
    }

    // ---------- tray ----------
    fn set_tray(&mut self, ctx: &egui::Context, on: bool) {
        if !on { self.tray = None; self.rec_item = None; win::show_from_tray(); return; }
        if self.tray.is_some() { return; }
        let menu = Menu::new();
        let show = MenuItem::with_id("show", "Show LiteRec", true, None);
        let rec = MenuItem::with_id("rec", "Start recording", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        let _ = menu.append_items(&[&show, &rec, &quit]);
        self.rec_item = Some(rec.clone());
        self.rec_item_text = "Start recording".into();
        let Ok(icon) = tray_icon::Icon::from_rgba(self.icon_small.clone(), 32, 32) else { return };
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("LiteRec")
            .with_icon(icon)
            .build();
        let Ok(tray) = tray else { return };

        let c = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                win::show_from_tray();
                c.request_repaint();
            }
        }));
        let (c, q, t, seen) = (ctx.clone(), self.quitting.clone(), self.tray_rec.clone(), self.quit_seen.clone());
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| match ev.id.0.as_str() {
            "show" => { win::show_from_tray(); c.request_repaint(); }
            "rec" => { t.store(true, Ordering::SeqCst); c.request_repaint(); }
            "quit" => {
                q.store(true, Ordering::SeqCst);
                c.send_viewport_cmd(egui::ViewportCommand::Close);
                c.request_repaint();
                let seen = seen.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(3));
                    if !seen.load(Ordering::SeqCst) { std::process::exit(0); }
                });
            }
            _ => {}
        }));
        self.tray = Some(tray);
    }

    // ================= UI sections =================
    fn ui_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Settings").clicked() { self.show_settings = true; ui.close_menu(); }
                if ui.button("Exit").clicked() {
                    self.quitting.store(true, Ordering::SeqCst);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    ui.close_menu();
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.preview, "Preview");
            });
            ui.menu_button("Help", |ui| {
                if ui.button("About").clicked() { self.show_settings = true; self.settings_tab = 4; ui.close_menu(); }
            });
        });
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let err = self.status.starts_with("Error");
            let col = if err { Color32::from_rgb(255, 110, 90) } else { ui.visuals().text_color() };
            ui.label(RichText::new(short(&self.status, 90)).color(col));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let total = self.last_frames + self.last_dropped;
                let pct = if total > 0 { self.last_dropped as f64 / total as f64 * 100.0 } else { 0.0 };
                ui.label(format!("Dropped frames: {} ({:.1}%)", self.last_dropped, pct));
                ui.separator();
                ui.label(format!("CPU: {:.0}%", self.cpu.pct));
                if self.rec.is_some() {
                    ui.separator();
                    let t = self.rec_secs;
                    ui.label(
                        RichText::new(format!("REC {:02}:{:02}:{:02}  {:.1} MB", t / 3600, (t / 60) % 60, t % 60, self.rec_size as f64 / 1_048_576.0))
                            .color(ACCENT).strong(),
                    );
                }
            });
        });
    }

    fn ui_preview(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let note = |txt: &str| painter.text(rect.center(), egui::Align2::CENTER_CENTER, txt, egui::FontId::proportional(16.0), Color32::GRAY);
        if !self.preview {
            note("Preview is off  (View > Preview)");
        } else if let Some(t) = &self.preview_tex {
            let aspect = t.size()[0] as f32 / t.size()[1] as f32;
            let (mut w, mut h) = (rect.width(), rect.width() / aspect);
            if h > rect.height() { h = rect.height(); w = h * aspect; }
            let r = egui::Rect::from_center_size(rect.center(), egui::vec2(w, h));
            painter.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        } else {
            note(if self.target().is_some() { "Waiting for first frame..." } else { "No source selected" });
        }
        if self.rec.is_some() {
            painter.circle_filled(rect.left_top() + egui::vec2(18.0, 18.0), 6.0, ACCENT);
            painter.text(rect.left_top() + egui::vec2(32.0, 18.0), egui::Align2::LEFT_CENTER, "REC", egui::FontId::proportional(14.0), Color32::WHITE);
        }
    }

    fn ui_docks(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let recording = self.rec.is_some();
        let busy = recording || self.finalizing.is_some();
        ui.columns(3, |cols| {
            // ---- Sources ----
            dock(&mut cols[0], "Sources", |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.kind, 0, "Display");
                        ui.radio_value(&mut self.kind, 1, "Window");
                    });
                    let wd = ui.available_width();
                    if self.kind == 0 {
                        let t = short(&self.displays.get(self.disp_idx).cloned().unwrap_or_default(), 40);
                        egui::ComboBox::from_id_salt("disp").width(wd).selected_text(t).show_ui(ui, |ui| {
                            for (i, n) in self.displays.iter().enumerate() { ui.selectable_value(&mut self.disp_idx, i, n); }
                        });
                        ui.checkbox(&mut self.use_dxgi, "No-border capture")
                            .on_hover_text("DXGI Desktop Duplication: avoids the yellow capture border. Untick to use Windows Graphics Capture.");
                    } else {
                        let t = short(&self.windows.get(self.win_idx).cloned().unwrap_or_default(), 40);
                        egui::ComboBox::from_id_salt("win").width(wd).selected_text(t).show_ui(ui, |ui| {
                            for (i, n) in self.windows.iter().enumerate() { ui.selectable_value(&mut self.win_idx, i, short(n, 60)); }
                        });
                        ui.small("Window capture may show a yellow border on older Windows 10.");
                    }
                    ui.checkbox(&mut self.use_cursor, "Capture cursor");
                    if ui.button("Refresh lists").clicked() { self.refresh_lists(); }
                });
            });
            // ---- Audio Mixer ----
            dock(&mut cols[1], "Audio Mixer", |ui| {
                egui::ScrollArea::vertical().id_salt("mixscroll").show(ui, |ui| {
                    let ds = if self.desk_dev.is_empty() { "Default device".to_string() } else { short(&self.desk_dev, 26) };
                    let ms = if self.mic_dev.is_empty() { "Default device".to_string() } else { short(&self.mic_dev, 26) };
                    let aps = self.app_sel.and_then(|i| self.apps.get(i)).map(|a| short(&a.label, 26)).unwrap_or_else(|| "(pick in Settings > Audio)".into());
                    mixer_channel(ui, "Desktop Audio", &ds, &mut self.use_desk, busy, &self.desk);
                    mixer_channel(ui, "Mic/Aux", &ms, &mut self.use_mic, busy, &self.mic);
                    mixer_channel(ui, "Application Audio", &aps, &mut self.use_app, busy, &self.app);
                    if let Some(e) = self.engine.err.lock().clone() {
                        ui.colored_label(Color32::from_rgb(230, 140, 60), e);
                    }
                });
            });
            // ---- Controls ----
            dock(&mut cols[2], "Controls", |ui| {
                let w = ui.available_width();
                if recording {
                    let b = egui::Button::new(RichText::new("Stop Recording").strong().color(Color32::WHITE)).fill(Color32::from_rgb(120, 30, 30));
                    if ui.add_sized([w, 34.0], b).clicked() { self.stop(); }
                } else {
                    let b = egui::Button::new(RichText::new("Start Recording").strong().color(Color32::WHITE)).fill(ACCENT);
                    let clicked = ui.add_enabled_ui(self.finalizing.is_none(), |ui| ui.add_sized([w, 34.0], b)).inner.clicked();
                    if clicked { self.start(); }
                }
                ui.add_space(4.0);
                ui.add_enabled_ui(!busy, |ui| {
                    ui.label("Quality preset");
                    self.preset_combo(ui, "ctl_preset", w);
                });
                ui.add_space(4.0);
                if ui.add_sized([w, 26.0], egui::Button::new("Settings")).clicked() { self.show_settings = true; }
                if ui.add_sized([w, 26.0], egui::Button::new("Exit")).clicked() {
                    self.quitting.store(true, Ordering::SeqCst);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }

    fn ui_settings(&mut self, ctx: &egui::Context) {
        if !self.show_settings { return; }
        let busy = self.rec.is_some() || self.finalizing.is_some();
        let mut open = true;
        let mut close = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .fixed_size([680.0, 400.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::SidePanel::left("snav").resizable(false).exact_width(110.0).show_inside(ui, |ui| {
                    for (i, n) in TABS.iter().enumerate() { ui.selectable_value(&mut self.settings_tab, i, *n); }
                });
                egui::TopBottomPanel::bottom("sfoot").show_inside(ui, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Close").clicked() { close = true; }
                        if busy { ui.label(RichText::new("Some options are locked while recording").small().weak()); }
                    });
                });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("setscroll").show(ui, |ui| {
                        ui.add_enabled_ui(!busy || self.settings_tab == 0 || self.settings_tab == 4, |ui| {
                            match self.settings_tab {
                                0 => {
                                    ui.checkbox(&mut self.preview, "Show live preview");
                                    egui::Grid::new("prev").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                                        ui.label("Preview quality");
                                        egui::ComboBox::from_id_salt("pq").selected_text(PREV_W[self.prev_q].0).show_ui(ui, |ui| {
                                            for (i, q) in PREV_W.iter().enumerate() { ui.selectable_value(&mut self.prev_q, i, q.0); }
                                        });
                                        ui.end_row();
                                        ui.label("Preview FPS");
                                        egui::ComboBox::from_id_salt("pf").selected_text(PREV_FPS[self.prev_fps].to_string()).show_ui(ui, |ui| {
                                            for (i, f) in PREV_FPS.iter().enumerate() { ui.selectable_value(&mut self.prev_fps, i, f.to_string()); }
                                        });
                                        ui.end_row();
                                    });
                                    ui.small("Lower quality / FPS = less CPU. Only affects the preview, never the recording. While recording, capture runs at the recording FPS.");
                                    ui.add_space(6.0);
                                    if ui.checkbox(&mut self.tray_mode, "Tray mode (close button hides to tray)").changed() {
                                        let on = self.tray_mode;
                                        self.set_tray(ctx, on);
                                    }
                                    ui.small("With tray mode on, the close button sends LiteRec to the tray. Use the tray icon to show it again or quit.");
                                    ui.add_space(6.0);
                                    ui.checkbox(&mut self.hide_self, "Hide LiteRec's own window from the preview and recordings");
                                    ui.small("Prevents the mirror-in-a-mirror effect when you capture the display LiteRec is on.");
                                }
                                1 => {
                                    egui::Grid::new("out").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                                        ui.label("Recording folder");
                                        ui.horizontal(|ui| {
                                            ui.add(egui::TextEdit::singleline(&mut self.out_dir).desired_width(260.0));
                                            if ui.button("Browse...").clicked() {
                                                if let Some(d) = rfd::FileDialog::new().set_directory(&self.out_dir).pick_folder() {
                                                    self.out_dir = d.display().to_string();
                                                }
                                            }
                                        });
                                        ui.end_row();
                                        ui.label("File type");
                                        ui.horizontal(|ui| {
                                            ui.radio_value(&mut self.mkv, true, "MKV (safest)");
                                            ui.radio_value(&mut self.mkv, false, "MP4 (most compatible)");
                                        });
                                        ui.end_row();
                                        ui.label("Quality preset");
                                        self.preset_combo(ui, "set_preset", 220.0);
                                        ui.end_row();
                                        ui.label("x264 preset");
                                        egui::ComboBox::from_id_salt("pre").selected_text(PRESETS[self.preset_idx]).show_ui(ui, |ui| {
                                            for (i, pr) in PRESETS.iter().enumerate() { ui.selectable_value(&mut self.preset_idx, i, *pr); }
                                        });
                                        ui.end_row();
                                        ui.label("Quality (CRF)");
                                        ui.add(egui::Slider::new(&mut self.crf, 18..=34));
                                        ui.end_row();
                                        ui.label("Encoder threads");
                                        ui.add(egui::Slider::new(&mut self.threads, 1..=8));
                                        ui.end_row();
                                    });
                                    ui.add_space(6.0);
                                    ui.label("Tip: higher CRF = smaller/faster.");
                                }
                                2 => {
                                    egui::Grid::new("vid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                                        ui.label("Output size");
                                        egui::ComboBox::from_id_salt("res").selected_text(RES[self.res_idx].0).show_ui(ui, |ui| {
                                            for (i, r) in RES.iter().enumerate() { ui.selectable_value(&mut self.res_idx, i, r.0); }
                                        });
                                        ui.end_row();
                                        ui.label("FPS");
                                        egui::ComboBox::from_id_salt("fps").selected_text(FPS[self.fps_idx].to_string()).show_ui(ui, |ui| {
                                            for (i, f) in FPS.iter().enumerate() { ui.selectable_value(&mut self.fps_idx, i, f.to_string()); }
                                        });
                                        ui.end_row();
                                    });
                                    ui.add_space(6.0);
                                    ui.checkbox(&mut self.use_cursor, "Capture cursor");
                                    ui.checkbox(&mut self.use_dxgi, "No-border display capture (DXGI Desktop Duplication)");
                                    ui.small("Untick to use Windows Graphics Capture for displays. Windows always use Graphics Capture.");
                                }
                                3 => {
                                    egui::Grid::new("aud").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                                        ui.label("Desktop audio device");
                                        dev_combo(ui, "ddev", &mut self.desk_dev, &self.out_devs);
                                        ui.end_row();
                                        ui.label("Microphone");
                                        dev_combo(ui, "mdev", &mut self.mic_dev, &self.in_devs);
                                        ui.end_row();
                                        ui.label("Application");
                                        let t = self.app_sel.and_then(|i| self.apps.get(i)).map(|a| short(&a.label, 40)).unwrap_or_else(|| "(none selected)".into());
                                        egui::ComboBox::from_id_salt("adev").width(300.0).selected_text(t).show_ui(ui, |ui| {
                                            ui.selectable_value(&mut self.app_sel, None, "(none selected)");
                                            for (i, a) in self.apps.iter().enumerate() { ui.selectable_value(&mut self.app_sel, Some(i), short(&a.label, 50)); }
                                        });
                                        ui.end_row();
                                    });
                                    ui.add_space(6.0);
                                    if ui.button("Refresh devices and apps").clicked() { self.refresh_lists(); }
                                    ui.small("Tick the matching source in the Audio Mixer to use it. 'Application' needs Windows 10 2004+; untick Desktop when using it to avoid doubling.");
                                }
                                _ => {
                                    ui.horizontal(|ui| {
                                        if let Some(t) = &self.logo_tex { ui.image((t.id(), egui::vec2(96.0, 96.0))); }
                                        ui.vertical(|ui| {
                                            ui.heading("LiteRec");
                                            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                                            ui.label("Lightweight screen recorder for older PCs.");
                                        });
                                    });
                                    ui.add_space(8.0);
                                    let fp = recorder::ffmpeg_path();
                                    if fp.is_absolute() && fp.exists() {
                                        ui.label(format!("ffmpeg: {}", fp.display()));
                                    } else {
                                        ui.label("ffmpeg: not next to literec.exe, will be looked up on PATH");
                                    }
                                    ui.label(format!("Settings file: {}", settings_path().display()));
                                }
                            }
                        });
                    });
                });
            });
        if close { open = false; }
        self.show_settings = open;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        self.frame_no += 1;
        if self.frame_no == 40 { win::dump_windows("startup +4s"); }
        if self.dump_after == Some(self.frame_no) { win::dump_windows("5s after X pressed"); self.dump_after = None; }
        if self.quitting.load(Ordering::SeqCst) {
            self.quit_seen.store(true, Ordering::SeqCst);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // close button -> tray
        if ctx.input(|i| i.viewport().close_requested()) && self.tray_mode && self.tray.is_some() && !self.quitting.load(Ordering::SeqCst) {
            win::dump_windows("X pressed (before hide)");
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            win::hide_to_tray();
            self.dump_after = Some(self.frame_no + 20);
        }
        if self.tray_mode && self.tray.is_none() { self.set_tray(ctx, true); }
        if self.hide_applied != Some(self.hide_self) && win::set_capture_excluded(self.hide_self) {
            self.hide_applied = Some(self.hide_self);
        }
        if let Some(item) = &self.rec_item {
            let want = if self.rec.is_some() { "Stop recording" } else { "Start recording" };
            if self.rec_item_text != want { item.set_text(want); self.rec_item_text = want.into(); }
        }
        // tray "start/stop"
        if self.tray_rec.swap(false, Ordering::SeqCst) {
            if self.rec.is_some() { self.stop(); } else if self.finalizing.is_none() { self.start(); }
        }

        // audio engine follows the UI
        let setup = self.audio_setup();
        if self.last_setup.as_ref() != Some(&setup) {
            self.engine.setup(setup.clone());
            self.last_setup = Some(setup);
        }

        // recorder / finalizer polling
        let mut stop_now = false;
        if let Some(r) = &self.rec {
            let s = r.stats();
            self.last_frames = s.frames;
            self.last_dropped = s.dropped;
            self.rec_secs = s.elapsed.as_secs();
            self.rec_size = s.size;
            if let Some(e) = s.err { self.status = format!("Error: {e}"); stop_now = true; }
        }
        if stop_now { let m = self.status.clone(); self.stop(); self.status = m; }
        if let Some((done, path)) = &self.finalizing {
            if done.load(Ordering::Relaxed) { self.status = format!("Saved: {path}"); self.finalizing = None; }
        }

        self.sync_session(false);
        self.update_preview(ctx);
        self.cpu.sample();
        let ms = if win::is_hidden() { 250 } else if self.preview { (1000 / PREV_FPS[self.prev_fps] as u64).min(100) } else { 100 };
        ctx.request_repaint_after(Duration::from_millis(ms));

        egui::TopBottomPanel::top("menu").show(ctx, |ui| self.ui_menu(ui, ctx));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| self.ui_status(ui));
        egui::TopBottomPanel::bottom("docks")
            .resizable(true)
            .default_height(240.0)
            .min_height(170.0)
            .show(ctx, |ui| self.ui_docks(ui, ctx));
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::from_rgb(10, 10, 12)))
            .show(ctx, |ui| self.ui_preview(ui));
        self.ui_settings(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
        if let Some(r) = self.rec.take() {
            let d = Arc::new(AtomicBool::new(false));
            r.stop(d.clone());
            self.finalizing = Some((d, String::new()));
        }
        if let Some((d, _)) = &self.finalizing {
            let t = Instant::now();
            while !d.load(Ordering::Relaxed) && t.elapsed() < Duration::from_secs(25) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let icon = image::load_from_memory(LOGO).ok().map(|i| {
        let rgba = i.to_rgba8();
        egui::IconData { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() }
    });
    let mut vp = egui::ViewportBuilder::default()
        .with_inner_size([980.0, 740.0])
        .with_min_inner_size([820.0, 560.0])
        .with_title("LiteRec");
    if let Some(i) = icon { vp = vp.with_icon(Arc::new(i)); }
    let opts = eframe::NativeOptions { viewport: vp, ..Default::default() };
    eframe::run_native("LiteRec", opts, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

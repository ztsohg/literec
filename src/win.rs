//! "Hide to tray" without really hiding the window.
//! Windows stops redrawing hidden windows, which would freeze the UI loop (tray menu actions,
//! recorder polling...). So instead the window is moved off-screen and removed from the taskbar /
//! Alt-Tab (tool-window style); the UI loop keeps running at a low rate.
use std::io::Write;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongPtrW, IsWindowVisible, IsZoomed, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    SetForegroundWindow, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, SW_HIDE, SW_MAXIMIZE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

const OFFSCREEN: i32 = -32000;
static HIDDEN: AtomicBool = AtomicBool::new(false);
static WAS_MAXIMIZED: AtomicBool = AtomicBool::new(false);
static HWND_CACHE: AtomicUsize = AtomicUsize::new(0);
static SAVED_X: AtomicI32 = AtomicI32::new(100);
static SAVED_Y: AtomicI32 = AtomicI32::new(100);

pub fn is_hidden() -> bool {
    HIDDEN.load(Ordering::SeqCst)
}

struct Find {
    pid: u32,
    found: HWND,
}

unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> i32 {
    let f = &mut *(lp as *mut Find);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == f.pid {
        let mut buf = [0u16; 64];
        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 64);
        if n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == "LiteRec" {
            f.found = hwnd;
            return 0; // stop
        }
    }
    1
}

/// Our own top-level window (matched by process id + title, so an Explorer window that happens
/// to be called "LiteRec" can never be picked).
fn main_window() -> Option<HWND> {
    let c = HWND_CACHE.load(Ordering::SeqCst);
    if c != 0 {
        return Some(c as HWND);
    }
    let mut f = Find { pid: unsafe { GetCurrentProcessId() }, found: null_mut() };
    unsafe { EnumWindows(Some(cb), &mut f as *mut _ as LPARAM) };
    if f.found.is_null() {
        None
    } else {
        HWND_CACHE.store(f.found as usize, Ordering::SeqCst);
        Some(f.found)
    }
}

pub fn hide_to_tray() {
    if is_hidden() { return; }
    let Some(h) = main_window() else { return };
    unsafe {
        // A maximized window can't be moved off-screen: Windows snaps it back to the monitor and it
        // stays visible as a tool window with a thin title bar. Restore it first, remember to re-maximize.
        let maximized = IsZoomed(h) != 0;
        WAS_MAXIMIZED.store(maximized, Ordering::SeqCst);
        if maximized {
            ShowWindow(h, SW_RESTORE);
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(h, &mut r) != 0 && r.left > OFFSCREEN + 1000 {
            SAVED_X.store(r.left, Ordering::SeqCst);
            SAVED_Y.store(r.top, Ordering::SeqCst);
        }
        // the style change only reaches the taskbar if the window is re-shown
        ShowWindow(h, SW_HIDE);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, (ex & !(WS_EX_APPWINDOW as isize)) | WS_EX_TOOLWINDOW as isize);
        SetWindowPos(h, null_mut(), OFFSCREEN, OFFSCREEN, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        ShowWindow(h, SW_SHOWNOACTIVATE);
    }
    HIDDEN.store(true, Ordering::SeqCst);
    dump_windows("after hide_to_tray");
}

pub fn show_from_tray() {
    let Some(h) = main_window() else { return };
    unsafe {
        if is_hidden() {
            ShowWindow(h, SW_HIDE);
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            SetWindowLongPtrW(h, GWL_EXSTYLE, (ex & !(WS_EX_TOOLWINDOW as isize)) | WS_EX_APPWINDOW as isize);
            SetWindowPos(h, null_mut(), SAVED_X.load(Ordering::SeqCst), SAVED_Y.load(Ordering::SeqCst), 0, 0, SWP_NOSIZE | SWP_NOZORDER);
            ShowWindow(h, SW_SHOW);
            if WAS_MAXIMIZED.swap(false, Ordering::SeqCst) {
                ShowWindow(h, SW_MAXIMIZE);
            }
        }
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetForegroundWindow(h);
    }
    HIDDEN.store(false, Ordering::SeqCst);
    dump_windows("after show_from_tray");
}

/// Keep LiteRec's own window out of screen captures (preview and recordings), which avoids the
/// "mirror in a mirror" effect. Returns false if the window doesn't exist yet (try again later).
/// Windows 10 2004+ makes the window fully invisible to capture; older builds show a black box.
pub fn set_capture_excluded(on: bool) -> bool {
    let Some(h) = main_window() else { return false };
    unsafe {
        if on {
            if SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE) == 0 {
                SetWindowDisplayAffinity(h, WDA_MONITOR);
            }
        } else {
            SetWindowDisplayAffinity(h, WDA_NONE);
        }
    }
    true
}

// ---------------- debug log: which windows does this process own? ----------------
struct Dump {
    pid: u32,
    lines: Vec<String>,
}

unsafe extern "system" fn dump_cb(hwnd: HWND, lp: LPARAM) -> i32 {
    let d = &mut *(lp as *mut Dump);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != d.pid {
        return 1;
    }
    let mut t = [0u16; 128];
    let n = GetWindowTextW(hwnd, t.as_mut_ptr(), 128);
    let mut c = [0u16; 128];
    let m = GetClassNameW(hwnd, c.as_mut_ptr(), 128);
    let mut r: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut r);
    d.lines.push(format!(
        "  hwnd={:?} visible={} class={:?} title={:?} rect=({},{})-({},{}) exstyle={:#x}",
        hwnd,
        IsWindowVisible(hwnd) != 0,
        String::from_utf16_lossy(&c[..m.max(0) as usize]),
        String::from_utf16_lossy(&t[..n.max(0) as usize]),
        r.left, r.top, r.right, r.bottom,
        GetWindowLongPtrW(hwnd, GWL_EXSTYLE)
    ));
    1
}

pub fn log_path() -> std::path::PathBuf {
    let base = std::env::var("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from("."));
    base.join("LiteRec").join("debug.log")
}

/// Appends the list of all top-level windows owned by this process to the debug log.
pub fn dump_windows(tag: &str) {
    let mut d = Dump { pid: unsafe { GetCurrentProcessId() }, lines: Vec::new() };
    unsafe { EnumWindows(Some(dump_cb), &mut d as *mut _ as LPARAM) };
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|x| x.as_secs()).unwrap_or(0);
    let path = log_path();
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] pid={} t={}  windows: {}", tag, d.pid, secs, d.lines.len());
        for l in &d.lines { let _ = writeln!(f, "{l}"); }
    }
}

//! Small Win32 helpers: list apps with windows (for per-app audio) and system CPU usage.
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetSystemTimes, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

#[derive(Clone)]
pub struct AppEntry {
    pub pid: u32,
    pub exe: String,
    pub label: String,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> i32 {
    let v = &mut *(lp as *mut Vec<(u32, String)>);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), 256);
    if n <= 0 {
        return 1;
    }
    let title = String::from_utf16_lossy(&buf[..n as usize]);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    v.push((pid, title));
    1
}

fn exe_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h as usize == 0 {
            return None;
        }
        let mut buf = [0u16; 520];
        let mut len = 520u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(|s| s.to_string())
    }
}

/// Visible top-level windows, one entry per process, excluding LiteRec itself.
pub fn list_apps() -> Vec<AppEntry> {
    let mut raw: Vec<(u32, String)> = Vec::new();
    unsafe {
        EnumWindows(Some(enum_cb), &mut raw as *mut _ as LPARAM);
    }
    let me = unsafe { GetCurrentProcessId() };
    let mut out: Vec<AppEntry> = Vec::new();
    for (pid, title) in raw {
        if pid == me || out.iter().any(|a| a.pid == pid) {
            continue;
        }
        if let Some(exe) = exe_name(pid) {
            let short: String = title.chars().take(45).collect();
            out.push(AppEntry { pid, label: format!("{exe} - {short}"), exe });
        }
    }
    out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    out
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

fn system_times() -> (u64, u64, u64) {
    unsafe {
        let mut i: FILETIME = std::mem::zeroed();
        let mut k: FILETIME = std::mem::zeroed();
        let mut u: FILETIME = std::mem::zeroed();
        GetSystemTimes(&mut i, &mut k, &mut u);
        (ft(i), ft(k), ft(u))
    }
}

/// Whole-system CPU usage, refreshed about once per second.
pub struct CpuMeter {
    last: Option<(u64, u64, u64)>,
    pub pct: f32,
    t: Instant,
}

impl CpuMeter {
    pub fn new() -> Self {
        Self { last: None, pct: 0.0, t: Instant::now() - Duration::from_secs(5) }
    }
    pub fn sample(&mut self) {
        if self.t.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.t = Instant::now();
        let (i, k, u) = system_times();
        if let Some((li, lk, lu)) = self.last {
            let idle = i.saturating_sub(li);
            let total = k.saturating_sub(lk) + u.saturating_sub(lu); // kernel time includes idle
            if total > 0 {
                self.pct = (total.saturating_sub(idle)) as f32 / total as f32 * 100.0;
            }
        }
        self.last = Some((i, k, u));
    }
}

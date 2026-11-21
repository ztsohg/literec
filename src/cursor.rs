//! Draws the mouse pointer onto frames from DXGI Desktop Duplication (which never includes it).
//! The pointer image is rendered once per cursor shape (on black and on white, to recover its
//! alpha) and then alpha-blended onto each frame: cheap enough for old CPUs.
use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, EnumDisplayMonitors, GdiFlush, GetObjectW,
    SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HDC, HMONITOR, RGBQUAD,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HCURSOR, ICONINFO,
};

struct Img {
    handle: usize,
    w: i32,
    h: i32,
    hx: i32,
    hy: i32,
    px: Vec<u8>, // BGRA: rgb premultiplied by alpha, a = coverage
}

#[derive(Default)]
pub struct Overlay {
    cache: Option<Img>,
}

unsafe extern "system" fn mon_cb(_m: HMONITOR, _dc: HDC, rc: *mut RECT, lp: LPARAM) -> i32 {
    let v = &mut *(lp as *mut Vec<RECT>);
    v.push(*rc);
    1
}

/// Top-left corner (in virtual-desktop coordinates) of the monitor that produced a `w` x `h` frame.
pub fn origin_for(idx: usize, w: usize, h: usize) -> (i32, i32) {
    let mut rects: Vec<RECT> = Vec::new();
    unsafe {
        EnumDisplayMonitors(null_mut(), std::ptr::null(), Some(mon_cb), &mut rects as *mut _ as LPARAM);
    }
    let fits = |m: &RECT| (m.right - m.left) as usize == w && (m.bottom - m.top) as usize == h;
    if let Some(m) = rects.get(idx).filter(|m| fits(m)) {
        return (m.left, m.top);
    }
    if let Some(m) = rects.iter().find(|m| fits(m)) {
        return (m.left, m.top);
    }
    (0, 0)
}

unsafe fn render(hc: HCURSOR) -> Option<Img> {
    let mut ii: ICONINFO = std::mem::zeroed();
    if GetIconInfo(hc, &mut ii) == 0 {
        return None;
    }
    let has_color = !ii.hbmColor.is_null();
    let src = if has_color { ii.hbmColor } else { ii.hbmMask };
    let mut bm: BITMAP = std::mem::zeroed();
    let got = GetObjectW(src as _, std::mem::size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut c_void);
    let (hx, hy) = (ii.xHotspot as i32, ii.yHotspot as i32);
    if has_color { DeleteObject(ii.hbmColor as _); }
    if !ii.hbmMask.is_null() { DeleteObject(ii.hbmMask as _); }
    let w = bm.bmWidth;
    let h = if has_color { bm.bmHeight } else { bm.bmHeight / 2 }; // mono cursors stack AND+XOR masks
    if got == 0 || w <= 0 || h <= 0 || w > 512 || h > 512 {
        return None;
    }

    let hdc = CreateCompatibleDC(null_mut());
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }],
    };
    let mut bits: *mut c_void = null_mut();
    let dib = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    if dib.is_null() || bits.is_null() {
        DeleteDC(hdc);
        return None;
    }
    let old = SelectObject(hdc, dib as _);
    let n = (w * h * 4) as usize;
    let buf = std::slice::from_raw_parts_mut(bits as *mut u8, n);

    buf.fill(0);
    DrawIconEx(hdc, 0, 0, hc, 0, 0, 0, null_mut(), DI_NORMAL);
    GdiFlush();
    let black = buf.to_vec();
    buf.fill(255);
    DrawIconEx(hdc, 0, 0, hc, 0, 0, 0, null_mut(), DI_NORMAL);
    GdiFlush();
    let white = buf.to_vec();

    SelectObject(hdc, old);
    DeleteObject(dib as _);
    DeleteDC(hdc);

    // alpha = 255 - (how much the white background shows through)
    let mut px = vec![0u8; n];
    for i in (0..n).step_by(4) {
        let d = (0..3).map(|c| white[i + c].saturating_sub(black[i + c])).max().unwrap_or(0);
        px[i..i + 3].copy_from_slice(&black[i..i + 3]);
        px[i + 3] = 255 - d;
    }
    Some(Img { handle: hc as usize, w, h, hx, hy, px })
}

impl Overlay {
    /// Blend the current pointer into `frame` (BGRA, `fw` x `fh`). `origin` = monitor top-left.
    pub fn draw(&mut self, frame: &mut [u8], fw: usize, fh: usize, origin: (i32, i32)) {
        unsafe {
            let mut ci: CURSORINFO = std::mem::zeroed();
            ci.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
            if GetCursorInfo(&mut ci) == 0 || (ci.flags & CURSOR_SHOWING) == 0 || ci.hCursor.is_null() {
                return;
            }
            let key = ci.hCursor as usize;
            if self.cache.as_ref().map(|c| c.handle) != Some(key) {
                self.cache = render(ci.hCursor).or(Some(Img { handle: key, w: 0, h: 0, hx: 0, hy: 0, px: Vec::new() }));
            }
            let Some(img) = &self.cache else { return };
            if img.w == 0 { return; }
            let x0 = ci.ptScreenPos.x - origin.0 - img.hx;
            let y0 = ci.ptScreenPos.y - origin.1 - img.hy;
            for cy in 0..img.h {
                let y = y0 + cy;
                if y < 0 || y >= fh as i32 { continue; }
                for cx in 0..img.w {
                    let x = x0 + cx;
                    if x < 0 || x >= fw as i32 { continue; }
                    let s = ((cy * img.w + cx) * 4) as usize;
                    let a = img.px[s + 3] as u32;
                    if a == 0 { continue; }
                    let d = (y as usize * fw + x as usize) * 4;
                    for c in 0..3 {
                        let v = img.px[s + c] as u32 + frame[d + c] as u32 * (255 - a) / 255;
                        frame[d + c] = v.min(255) as u8;
                    }
                }
            }
        }
    }
}

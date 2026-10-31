//! Click-through, always-on-top, per-pixel-alpha windows for the correction flash and squiggly underlines.

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

pub struct Layered {
    pub hwnd: HWND,
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCHITTEST {
            return LRESULT(-1); // HTTRANSPARENT
        }
        DefWindowProcW(h, msg, w, l)
    }
}

impl Layered {
    pub fn new() -> Self {
        unsafe {
            let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
            let class = w!("qalam_overlay");
            let wc = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: hinst, lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc); // fails harmlessly the second time
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!(""),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinst),
                None,
            )
            .unwrap_or_default();
            Layered { hwnd }
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Show `canvas` at screen position (x, y), faded by `alpha` (0-255).
    pub fn show(&self, x: i32, y: i32, canvas: &Canvas, alpha: u8) {
        if canvas.w <= 0 || canvas.h <= 0 {
            return self.hide();
        }
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: canvas.w,
                    biHeight: -canvas.h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            if let Ok(bmp) = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                std::ptr::copy_nonoverlapping(canvas.px.as_ptr(), bits as *mut u32, canvas.px.len());
                let old = SelectObject(mem, bmp.into());
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: alpha,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let dst = POINT { x, y };
                let size = SIZE { cx: canvas.w, cy: canvas.h };
                let src = POINT::default();
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    Some(screen),
                    Some(&dst),
                    Some(&size),
                    Some(mem),
                    Some(&src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
        }
    }
}

/// Premultiplied BGRA pixel buffer.
pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        Canvas { w: w.max(0), h: h.max(0), px: vec![0; (w.max(0) * h.max(0)) as usize] }
    }

    /// Blend colour (r, g, b) with coverage `a` (0..=1) over the pixel.
    pub fn plot(&mut self, x: i32, y: i32, (r, g, b): (u8, u8, u8), a: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let i = (y * self.w + x) as usize;
        let dst = self.px[i];
        let a = a.min(1.0);
        let mix = |src: u8, d: u32| ((src as f32 * a) + (d as f32) * (1.0 - a)).round() as u32;
        let (db, dg, dr, da) = (dst & 0xFF, (dst >> 8) & 0xFF, (dst >> 16) & 0xFF, dst >> 24);
        let na = ((255.0 * a) + da as f32 * (1.0 - a)).round() as u32;
        self.px[i] = (na << 24) | (mix(r, dr) << 16) | (mix(g, dg) << 8) | mix(b, db);
    }

    /// Filled rounded rectangle with soft edges.
    pub fn round_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, rad: f32, color: (u8, u8, u8), alpha: f32) {
        for y in (y0.floor() as i32)..(y1.ceil() as i32) {
            for x in (x0.floor() as i32)..(x1.ceil() as i32) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let cx = px.clamp(x0 + rad, x1 - rad);
                let cy = py.clamp(y0 + rad, y1 - rad);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let cover = (rad - d + 0.5).clamp(0.0, 1.0);
                self.plot(x, y, color, cover * alpha);
            }
        }
    }

    /// Squiggly underline from x0 to x1 with its centre line at y.
    pub fn squiggle(&mut self, x0: i32, x1: i32, y: f32, scale: f32, color: (u8, u8, u8)) {
        let amp = 1.4 * scale;
        let period = 4.0 * scale;
        let thick = 0.75 * scale.max(1.0);
        for x in x0..x1 {
            for sub in 0..4 {
                let fx = x as f32 + sub as f32 / 4.0;
                let phase = ((fx - x0 as f32) / period).fract();
                let tri = if phase < 0.5 { phase * 4.0 - 1.0 } else { 3.0 - phase * 4.0 };
                let cy = y + tri * amp;
                for yy in (cy - thick - 1.0).floor() as i32..=(cy + thick + 1.0).ceil() as i32 {
                    let d = (yy as f32 + 0.5 - cy).abs();
                    let cover = (thick - d + 0.5).clamp(0.0, 1.0) / 4.0;
                    self.plot(x, yy, color, cover * 3.2);
                }
            }
        }
    }
}

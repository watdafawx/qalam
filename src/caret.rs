//! Where is the text caret on screen? Tries the classic Win32 caret, then MSAA (Chrome, Electron...).

use std::ffi::c_void;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, GUITHREADINFO, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, OBJID_CARET,
};
use windows::core::Interface;

thread_local! {
    static LAST: std::cell::Cell<(isize, RECT)> = const { std::cell::Cell::new((0, RECT { left: 0, top: 0, right: 0, bottom: 0 })) };
}

/// Caret rectangle in screen pixels. Falls back to the last known caret in the same window, then the mouse.
/// Also says where the answer came from, for the debug log.
pub fn caret_rect() -> (RECT, &'static str) {
    unsafe {
        let fg = GetForegroundWindow();
        if let Some((r, src)) = query(fg) {
            LAST.set((fg.0 as isize, r));
            return (r, src);
        }
        if let Some(r) = uia_caret() {
            LAST.set((fg.0 as isize, r));
            return (r, "uia");
        }
        let (h, r) = LAST.get();
        if h == fg.0 as isize && r.bottom > r.top {
            return (r, "last");
        }
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        (RECT { left: p.x, top: p.y, right: p.x + 1, bottom: p.y + 20 }, "mouse")
    }
}

unsafe fn query(fg: HWND) -> Option<(RECT, &'static str)> {
    unsafe {
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gti = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        let have_gti = GetGUIThreadInfo(tid, &mut gti).is_ok();
        if have_gti && !gti.hwndCaret.is_invalid() {
            let r = gti.rcCaret;
            if r.bottom > r.top && !(r.left == 0 && r.top == 0) {
                let mut a = POINT { x: r.left, y: r.top };
                let mut b = POINT { x: r.right, y: r.bottom };
                let _ = ClientToScreen(gti.hwndCaret, &mut a);
                let _ = ClientToScreen(gti.hwndCaret, &mut b);
                return Some((RECT { left: a.x, top: a.y, right: b.x.max(a.x + 1), bottom: b.y }, "win32"));
            }
        }
        let target = if have_gti && !gti.hwndFocus.is_invalid() { gti.hwndFocus } else { fg };
        let mut raw: *mut c_void = std::ptr::null_mut();
        AccessibleObjectFromWindow(target, OBJID_CARET.0 as u32, &IAccessible::IID, &mut raw).ok()?;
        if raw.is_null() {
            return None;
        }
        let acc = IAccessible::from_raw(raw);
        let (mut x, mut y, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
        acc.accLocation(&mut x, &mut y, &mut w, &mut h, &VARIANT::from(CHILDID_SELF as i32)).ok()?;
        if h <= 0 || (x == 0 && y == 0) {
            return None;
        }
        Some((RECT { left: x, top: y, right: x + w.max(1), bottom: y + h }, "msaa"))
    }
}

thread_local! {
    static UIA: std::cell::RefCell<Option<windows::Win32::UI::Accessibility::IUIAutomation>> = const { std::cell::RefCell::new(None) };
}

/// Caret via UI Automation (Chromium/Electron apps that don't expose the classic caret).
fn uia_caret() -> Option<RECT> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::UI::Accessibility::*;
    unsafe {
        let auto = UIA.with_borrow_mut(|u| {
            if u.is_none() {
                *u = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok();
            }
            u.clone()
        })?;
        let el = auto.GetFocusedElement().ok()?;
        let tp2: IUIAutomationTextPattern2 = el.GetCurrentPatternAs(UIA_TextPattern2Id).ok()?;
        let mut active = windows::core::BOOL(0);
        let caret = tp2.GetCaretRange(&mut active).ok()?;
        // A collapsed range often has no rectangle: measure the character before the caret.
        let r = caret.Clone().ok()?;
        let _ = r.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1);
        let psa = r.GetBoundingRectangles().ok()?;
        if psa.is_null() {
            return None;
        }
        use windows::Win32::System::Ole::*;
        let mut out = None;
        if let (Ok(lb), Ok(ub)) = (SafeArrayGetLBound(psa, 1), SafeArrayGetUBound(psa, 1)) {
            let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
            if ub - lb + 1 >= 4 && SafeArrayAccessData(psa, &mut data).is_ok() {
                let v = std::slice::from_raw_parts(data as *const f64, (ub - lb + 1) as usize);
                let n = v.len() / 4 * 4;
                let c = &v[n - 4..n]; // last line's rect
                if c[3] > 0.0 {
                    let right = (c[0] + c[2]) as i32;
                    out = Some(RECT { left: right, top: c[1] as i32, right: right + 1, bottom: (c[1] + c[3]) as i32 });
                }
                let _ = SafeArrayUnaccessData(psa);
            }
        }
        let _ = SafeArrayDestroy(psa);
        out
    }
}

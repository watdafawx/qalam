//! The Apps window: blacklist/whitelist mode, per-app rules, and a picker for running apps.

use crate::editor::{alert, child, scale};
use crate::{app_icon, lock, process_name_pid, reload_config, shared, wide};
use qalam::config;
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OPENFILENAMEW};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, PWSTR, w};

const ID_BLACK: i32 = 200;
const ID_WHITE: i32 = 201;
const ID_LIST: i32 = 202;
const ID_ADD: i32 = 203;
const ID_BROWSE: i32 = 204;
const ID_RULE: i32 = 205;
const ID_REMOVE: i32 = 206;

const HINT_BLACK: &str = "qalam runs everywhere except the apps below. \u{201c}Replacements only\u{201d} keeps your text replacements but \
                          turns off the popup, autocorrect and underlines (terminals default to this so arrow keys reach the shell).";
const HINT_WHITE: &str = "qalam runs only in the apps below. Everything else is left alone. \u{201c}Replacements only\u{201d} keeps \
                          text replacements but turns off the popup, autocorrect and underlines.";

struct Win {
    hwnd: HWND,
    black: HWND,
    white: HWND,
    hint: HWND,
    list: HWND,
    add: HWND,
    browse: HWND,
    rule: HWND,
    remove: HWND,
    font: HFONT,
    /// (app, replacements_only) for each list row.
    rows: Vec<(String, bool)>,
}

thread_local! {
    static W: RefCell<Option<Win>> = const { RefCell::new(None) };
    static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn hwnd() -> Option<HWND> {
    W.with_borrow(|w| w.as_ref().map(|w| w.hwnd))
}

fn whitelist(g: &config::General) -> bool {
    g.app_mode.eq_ignore_ascii_case("whitelist")
}

fn has(list: &[String], app: &str) -> bool {
    list.iter().any(|a| a.eq_ignore_ascii_case(app))
}

fn without(list: &[String], app: &str) -> Vec<String> {
    list.iter().filter(|a| !a.eq_ignore_ascii_case(app)).cloned().collect()
}

fn with(list: &[String], app: &str) -> Vec<String> {
    let mut v = without(list, app);
    v.push(app.to_string());
    v
}

fn toml_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|i| format!("\"{}\"", i.replace('\\', "\\\\").replace('"', "\\\""))).collect::<Vec<_>>().join(", "))
}

/// Write app settings to config.toml and apply them.
fn save(changes: &[(&str, String)]) {
    let r: Result<(), String> = changes
        .iter()
        .try_for_each(|(k, v)| config::set_general(k, v).map_err(|e| e.to_string()))
        .and_then(|_| reload_config());
    if let Err(e) = r {
        if let Some(h) = hwnd() {
            alert(h, &format!("Couldn't save: {e}"));
        }
    }
    crate::reapply_foreground();
}

fn general() -> config::General {
    lock(&shared().engine).cfg.general.clone()
}

/// Add `app` to the active list (Off in blacklist mode, Full in whitelist mode).
pub fn add_app(app: &str) {
    let g = general();
    if whitelist(&g) {
        save(&[("enabled_apps", toml_list(&with(&g.enabled_apps, app)))]);
    } else {
        save(&[
            ("disabled_apps", toml_list(&with(&g.disabled_apps, app))),
            ("replacements_only_apps", toml_list(&without(&g.replacements_only_apps, app))),
        ]);
    }
    reload_rows();
}

/// Tray shortcut: is `app` currently switched off?
pub fn is_off(app: &str) -> bool {
    let g = general();
    if whitelist(&g) { !has(&g.enabled_apps, app) } else { has(&g.disabled_apps, app) }
}

/// Tray shortcut: switch qalam off/on for `app` in whichever mode is active.
pub fn toggle_off(app: &str) {
    let g = general();
    let off = is_off(app);
    if whitelist(&g) {
        let list = if off { with(&g.enabled_apps, app) } else { without(&g.enabled_apps, app) };
        save(&[("enabled_apps", toml_list(&list))]);
    } else {
        let list = if off { without(&g.disabled_apps, app) } else { with(&g.disabled_apps, app) };
        save(&[("disabled_apps", toml_list(&list))]);
    }
    reload_rows();
}

fn rows_for(g: &config::General) -> Vec<(String, bool)> {
    let mut rows: Vec<(String, bool)> = if whitelist(g) {
        g.enabled_apps.iter().map(|a| (a.clone(), has(&g.replacements_only_apps, a))).collect()
    } else {
        let mut v: Vec<(String, bool)> = g.disabled_apps.iter().map(|a| (a.clone(), false)).collect();
        v.extend(g.replacements_only_apps.iter().filter(|a| !has(&g.disabled_apps, a)).map(|a| (a.clone(), true)));
        v
    };
    rows.sort_by_key(|(a, _)| a.to_lowercase());
    rows
}

fn reload_rows() {
    let Some((list, black, white, hint)) = W.with_borrow(|w| w.as_ref().map(|w| (w.list, w.black, w.white, w.hint))) else {
        return;
    };
    let g = general();
    let wl = whitelist(&g);
    let rows = rows_for(&g);
    unsafe {
        SendMessageW(black, BM_SETCHECK, Some(WPARAM(if wl { 0 } else { 1 })), None);
        SendMessageW(white, BM_SETCHECK, Some(WPARAM(if wl { 1 } else { 0 })), None);
        let t = wide(if wl { HINT_WHITE } else { HINT_BLACK });
        let _ = SetWindowTextW(hint, PCWSTR(t.as_ptr()));
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        for (i, (app, repl)) in rows.iter().enumerate() {
            let rule = match (wl, repl) {
                (_, true) => "Replacements only",
                (true, false) => "Full",
                (false, false) => "Off",
            };
            for (col, cell) in [app.as_str(), rule].iter().enumerate() {
                let mut t = wide(cell);
                let item = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: i as i32,
                    iSubItem: col as i32,
                    pszText: PWSTR(t.as_mut_ptr()),
                    ..Default::default()
                };
                let msg = if col == 0 { LVM_INSERTITEMW } else { LVM_SETITEMTEXTW };
                SendMessageW(list, msg, Some(WPARAM(i)), Some(LPARAM(&item as *const _ as isize)));
            }
        }
    }
    W.with_borrow_mut(|w| {
        if let Some(w) = w {
            w.rows = rows;
        }
    });
    update_buttons();
}

fn selected() -> Option<(String, bool)> {
    let (list, rows) = W.with_borrow(|w| w.as_ref().map(|w| (w.list, w.rows.clone())))?;
    let i = unsafe { SendMessageW(list, LVM_GETNEXTITEM, Some(WPARAM(usize::MAX)), Some(LPARAM(LVNI_SELECTED as isize))).0 };
    if i < 0 { None } else { rows.get(i as usize).cloned() }
}

fn update_buttons() {
    let sel = selected().is_some();
    W.with_borrow(|w| {
        if let Some(w) = w {
            unsafe {
                let _ = EnableWindow(w.rule, sel);
                let _ = EnableWindow(w.remove, sel);
            }
        }
    });
}

fn change_rule() {
    let Some((app, repl)) = selected() else { return };
    let g = general();
    if whitelist(&g) {
        let list = if repl { without(&g.replacements_only_apps, &app) } else { with(&g.replacements_only_apps, &app) };
        save(&[("replacements_only_apps", toml_list(&list))]);
    } else if repl {
        save(&[
            ("replacements_only_apps", toml_list(&without(&g.replacements_only_apps, &app))),
            ("disabled_apps", toml_list(&with(&g.disabled_apps, &app))),
        ]);
    } else {
        save(&[
            ("disabled_apps", toml_list(&without(&g.disabled_apps, &app))),
            ("replacements_only_apps", toml_list(&with(&g.replacements_only_apps, &app))),
        ]);
    }
    reload_rows();
}

fn remove() {
    let Some((app, _)) = selected() else { return };
    let g = general();
    if whitelist(&g) {
        save(&[("enabled_apps", toml_list(&without(&g.enabled_apps, &app)))]);
    } else {
        save(&[
            ("disabled_apps", toml_list(&without(&g.disabled_apps, &app))),
            ("replacements_only_apps", toml_list(&without(&g.replacements_only_apps, &app))),
        ]);
    }
    reload_rows();
}

fn set_mode(wl: bool) {
    if whitelist(&general()) == wl {
        return;
    }
    save(&[("app_mode", if wl { "\"whitelist\"".into() } else { "\"blacklist\"".into() })]);
    reload_rows();
}

/// Apps with a visible window right now: (exe name, a window title).
fn running_apps() -> Vec<(String, String)> {
    unsafe extern "system" fn each(h: HWND, l: LPARAM) -> BOOL {
        unsafe {
            let out = &mut *(l.0 as *mut Vec<(String, String)>);
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            if !IsWindowVisible(h).as_bool()
                || GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid())
                || ex & WS_EX_TOOLWINDOW.0 != 0
            {
                return true.into();
            }
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(h, &mut buf);
            if n <= 0 {
                return true.into();
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == std::process::id() {
                return true.into();
            }
            if let Some(name) = process_name_pid(pid)
                && !out.iter().any(|(a, _)| a.eq_ignore_ascii_case(&name))
                && !name.eq_ignore_ascii_case("explorer.exe")
                && !name.eq_ignore_ascii_case("TextInputHost.exe")
                && !name.eq_ignore_ascii_case("ApplicationFrameHost.exe")
            {
                out.push((name, String::from_utf16_lossy(&buf[..n as usize])));
            }
            true.into()
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut out as *mut _ as isize));
    }
    out.sort_by_key(|(a, _)| a.to_lowercase());
    out
}

fn pick_running(owner: HWND) {
    let apps = running_apps();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (i, (name, title)) in apps.iter().enumerate() {
            let title: String = title.chars().take(50).collect();
            let t = wide(&format!("{name}\t{title}"));
            let _ = AppendMenuW(menu, MF_STRING, 1000 + i, PCWSTR(t.as_ptr()));
        }
        if apps.is_empty() {
            let _ = AppendMenuW(menu, MF_GRAYED, 0, w!("No other apps with windows"));
        }
        let mut r = RECT::default();
        let add = W.with_borrow(|w| w.as_ref().map(|w| w.add)).unwrap_or(owner);
        let _ = GetWindowRect(add, &mut r);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, r.left, r.bottom, Some(0), owner, None).0 as usize;
        let _ = DestroyMenu(menu);
        if let Some((name, _)) = cmd.checked_sub(1000).and_then(|i| apps.get(i)) {
            add_app(name);
        }
    }
}

fn browse(owner: HWND) {
    let mut file = [0u16; 1024];
    let filter = wide("Programs (*.exe)\0*.exe\0\0");
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: w!("Pick an app"),
        Flags: OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        let n = file.iter().position(|&c| c == 0).unwrap_or(0);
        let path = String::from_utf16_lossy(&file[..n]);
        if let Some(name) = path.rsplit('\\').next().filter(|s| !s.is_empty()) {
            add_app(name);
        }
    }
}

pub fn open() {
    unsafe {
        if let Some(h) = hwnd() {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
            return;
        }
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        if !REGISTERED.get() {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: hinst,
                lpszClassName: w!("qalam_apps"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: app_icon(),
                hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
                ..Default::default()
            };
            RegisterClassW(&wc);
            REGISTERED.set(true);
        }
        let Ok(h) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("qalam_apps"),
            w!("qalam · Apps"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            100,
            100,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };
        let s = scale(h);
        let (wd, ht) = ((600.0 * s) as i32, (500.0 * s) as i32);
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let _ = SetWindowPos(h, None, wa.left + (wa.right - wa.left - wd) / 2, wa.top + (wa.bottom - wa.top - ht) / 2, wd, ht, SWP_NOZORDER);
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
    }
}

unsafe fn create(h: HWND) {
    unsafe {
        let s = scale(h);
        let font = CreateFontW(-((12.0 * s) as i32), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let button = w!("BUTTON");
        let black = child(h, button, "Blacklist: run everywhere except these apps", (BS_AUTORADIOBUTTON as u32) | WS_TABSTOP.0 | WS_GROUP.0, 0, ID_BLACK);
        let white = child(h, button, "Whitelist: run only in these apps", BS_AUTORADIOBUTTON as u32, 0, ID_WHITE);
        let hint = child(h, w!("STATIC"), "", 0, 0, 0);
        let list = child(h, w!("SysListView32"), "", (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS) as u32 | WS_TABSTOP.0 | WS_BORDER.0, 0, ID_LIST);
        let ex = (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize;
        SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(ex as usize)), Some(LPARAM(ex)));
        for (i, (name, width)) in [("App", 300.0), ("Rule", 180.0)].iter().enumerate() {
            let mut t = wide(name);
            let col = LVCOLUMNW { mask: LVCF_TEXT | LVCF_WIDTH, cx: (width * s) as i32, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
            SendMessageW(list, LVM_INSERTCOLUMNW, Some(WPARAM(i)), Some(LPARAM(&col as *const _ as isize)));
        }
        let add = child(h, button, "Add running app...", WS_TABSTOP.0 | WS_GROUP.0, 0, ID_ADD);
        let browse = child(h, button, "Browse...", WS_TABSTOP.0, 0, ID_BROWSE);
        let rule = child(h, button, "Change rule", WS_TABSTOP.0, 0, ID_RULE);
        let remove = child(h, button, "Remove", WS_TABSTOP.0, 0, ID_REMOVE);
        for c in [black, white, hint, list, add, browse, rule, remove] {
            SendMessageW(c, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }
        W.set(Some(Win { hwnd: h, black, white, hint, list, add, browse, rule, remove, font, rows: Vec::new() }));
        reload_rows();
    }
}

unsafe fn layout(h: HWND) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(h, &mut rc);
        let s = scale(h);
        let px = |v: f32| (v * s) as i32;
        let (cw, ch) = (rc.right, rc.bottom);
        let m = px(12.0);
        W.with_borrow(|w| {
            let Some(w) = w else { return };
            let mv = |c: HWND, x, y, wd, ht| {
                let _ = MoveWindow(c, x, y, wd, ht, true);
            };
            mv(w.black, m, m, cw - 2 * m, px(24.0));
            mv(w.white, m, m + px(26.0), cw - 2 * m, px(24.0));
            mv(w.hint, m, m + px(58.0), cw - 2 * m, px(52.0));
            let list_top = m + px(116.0);
            let btn_h = px(30.0);
            mv(w.list, m, list_top, cw - 2 * m, (ch - list_top - m * 2 - btn_h).max(px(60.0)));
            SendMessageW(w.list, LVM_SETCOLUMNWIDTH, Some(WPARAM(0)), Some(LPARAM((cw - 2 * m - px(210.0)) as isize)));
            let y = ch - m - btn_h;
            let bw = [px(150.0), px(96.0), px(110.0), px(96.0)];
            let mut x = m;
            for (c, wd) in [w.add, w.browse, w.rule, w.remove].iter().zip(bw) {
                mv(*c, x, y, wd, btn_h);
                x += wd + px(8.0);
            }
        });
    }
}

unsafe extern "system" fn proc_(h: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                create(h);
                LRESULT(0)
            }
            WM_SIZE => {
                layout(h);
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let s = scale(h);
                let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT { x: (500.0 * s) as i32, y: (380.0 * s) as i32 };
                LRESULT(0)
            }
            WM_COMMAND => {
                match (wparam.0 & 0xFFFF) as i32 {
                    ID_BLACK => set_mode(false),
                    ID_WHITE => set_mode(true),
                    ID_ADD => pick_running(h),
                    ID_BROWSE => browse(h),
                    ID_RULE => change_rule(),
                    ID_REMOVE => remove(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_NOTIFY => {
                let hdr = &*(lparam.0 as *const NMHDR);
                if hdr.idFrom == ID_LIST as usize {
                    if hdr.code == LVN_ITEMCHANGED {
                        update_buttons();
                    } else if hdr.code == NM_DBLCLK {
                        change_rule();
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(h);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(w) = W.take() {
                    let _ = DeleteObject(w.font.into());
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, wparam, lparam),
        }
    }
}

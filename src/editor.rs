//! The Replacements window: list of triggers, and a form to add, change or delete one.

use crate::{app_icon, reload_config, wide};
use std::cell::RefCell;
use qalam::config::{self, dpapi};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, PWSTR, w};

const ID_LIST: i32 = 100;
const ID_TRIGGER: i32 = 101;
const ID_TEXT: i32 = 102;
const ID_SECRET: i32 = 103;
const ID_NEW: i32 = 104;
const ID_SAVE: i32 = 105;
const ID_DELETE: i32 = 106;

const HINT_DEFAULT: &str =
    "Type the trigger then space, Enter or punctuation in any app. Secrets are encrypted with your Windows login.";
const HINT_SECRET: &str = "Secret text is hidden. Leave it blank to keep the current value, or type a new one.";

struct Row {
    trigger: String,
    text: Option<String>, // None for secrets
}

struct Editor {
    hwnd: HWND,
    list: HWND,
    trigger: HWND,
    text: HWND,
    secret: HWND,
    new: HWND,
    save: HWND,
    delete: HWND,
    hint: HWND,
    labels: [HWND; 2],
    font: HFONT,
    rows: Vec<Row>,
    /// Trigger of the row being edited (None = adding a new one).
    editing: Option<String>,
}

thread_local! {
    static ED: RefCell<Option<Editor>> = const { RefCell::new(None) };
    static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn hwnd() -> Option<HWND> {
    ED.with_borrow(|e| e.as_ref().map(|e| e.hwnd))
}

fn scale(h: HWND) -> f32 {
    unsafe { GetDpiForWindow(h).max(96) as f32 / 96.0 }
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
            let icc = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
            };
            let _ = InitCommonControlsEx(&icc);
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: hinst,
                lpszClassName: w!("qalam_editor"),
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
            w!("qalam_editor"),
            w!("qalam · Replacements"),
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
        // Size for the window's DPI and centre on its monitor.
        let s = scale(h);
        let (w, hgt) = ((720.0 * s) as i32, (540.0 * s) as i32);
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let _ = SetWindowPos(
            h,
            None,
            wa.left + (wa.right - wa.left - w) / 2,
            wa.top + (wa.bottom - wa.top - hgt) / 2,
            w,
            hgt,
            SWP_NOZORDER,
        );
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
        SetFocus(ED.with_borrow(|e| e.as_ref().map(|e| e.trigger))).ok();
    }
}

unsafe fn child(parent: HWND, class: PCWSTR, text: &str, style: u32, ex: u32, id: i32) -> HWND {
    unsafe {
        let t = wide(text);
        CreateWindowExW(
            WINDOW_EX_STYLE(ex),
            class,
            PCWSTR(t.as_ptr()),
            WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
            0,
            0,
            10,
            10,
            Some(parent),
            Some(HMENU(id as usize as _)),
            None,
            None,
        )
        .unwrap_or_default()
    }
}

unsafe fn create(h: HWND) {
    unsafe {
        let s = scale(h);
        let font = CreateFontW(
            -((12.0 * s) as i32),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let list = child(
            h,
            w!("SysListView32"),
            "",
            (LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS) as u32 | WS_TABSTOP.0 | WS_BORDER.0,
            0,
            ID_LIST,
        );
        let ex = (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize;
        SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(ex as usize)), Some(LPARAM(ex)));
        for (i, (name, width)) in [("Trigger", 150.0), ("Replaced with", 380.0), ("Type", 80.0)].iter().enumerate() {
            let mut t = wide(name);
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: (width * s) as i32,
                pszText: PWSTR(t.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(list, LVM_INSERTCOLUMNW, Some(WPARAM(i)), Some(LPARAM(&col as *const _ as isize)));
        }
        let static_ = w!("STATIC");
        let edit = w!("EDIT");
        let button = w!("BUTTON");
        let l1 = child(h, static_, "Trigger", 0, 0, 0);
        let trigger = child(h, edit, "", (ES_AUTOHSCROLL as u32) | WS_TABSTOP.0, WS_EX_CLIENTEDGE.0, ID_TRIGGER);
        SendMessageW(trigger, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(w!("e.g. @@addr").as_ptr() as isize)));
        let secret = child(
            h,
            button,
            "Secret (password, ID): stored encrypted, hidden here",
            (BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0,
            0,
            ID_SECRET,
        );
        let l2 = child(h, static_, "Replace with", 0, 0, 0);
        let text = child(
            h,
            edit,
            "",
            (ES_MULTILINE | ES_WANTRETURN | ES_AUTOVSCROLL) as u32 | WS_VSCROLL.0 | WS_TABSTOP.0,
            WS_EX_CLIENTEDGE.0,
            ID_TEXT,
        );
        let hint = child(h, static_, HINT_DEFAULT, 0, 0, 0);
        let new = child(h, button, "New", WS_TABSTOP.0, 0, ID_NEW);
        let save = child(h, button, "Save", (BS_DEFPUSHBUTTON as u32) | WS_TABSTOP.0, 0, ID_SAVE);
        let delete = child(h, button, "Delete", WS_TABSTOP.0, 0, ID_DELETE);
        for c in [list, l1, trigger, secret, l2, text, hint, new, save, delete] {
            SendMessageW(c, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }
        ED.set(Some(Editor {
            hwnd: h,
            list,
            trigger,
            text,
            secret,
            new,
            save,
            delete,
            hint,
            labels: [l1, l2],
            font,
            rows: Vec::new(),
            editing: None,
        }));
        reload_rows(None);
        update_buttons();
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
        let label_w = px(96.0);
        let row_h = px(26.0);
        let text_h = px(96.0);
        let btn_w = px(88.0);
        let btn_h = px(30.0);
        let panel_h = row_h + px(10.0) + text_h + px(12.0) + btn_h;
        ED.with_borrow(|e| {
            let Some(e) = e else { return };
            let mv = |c: HWND, x, y, w, hh| {
                let _ = MoveWindow(c, x, y, w, hh, true);
            };
            let list_h = (ch - m * 3 - panel_h).max(px(80.0));
            mv(e.list, m, m, cw - 2 * m, list_h);
            let widths = [px(150.0), 0, px(80.0)];
            let mid = (cw - 2 * m - widths[0] - widths[2] - px(24.0)).max(px(120.0));
            SendMessageW(e.list, LVM_SETCOLUMNWIDTH, Some(WPARAM(1)), Some(LPARAM(mid as isize)));

            let mut y = m + list_h + m;
            mv(e.labels[0], m, y + px(4.0), label_w, row_h);
            mv(e.trigger, m + label_w, y, px(200.0), row_h);
            mv(e.secret, m + label_w + px(216.0), y, cw - m - (m + label_w + px(216.0)), row_h);
            y += row_h + px(10.0);
            mv(e.labels[1], m, y + px(4.0), label_w, row_h);
            mv(e.text, m + label_w, y, cw - 2 * m - label_w, text_h);
            y += text_h + px(12.0);
            let bx = cw - m - btn_w * 3 - px(16.0);
            mv(e.hint, m, y, (bx - m - px(8.0)).max(px(50.0)), btn_h + px(8.0));
            mv(e.new, bx, y, btn_w, btn_h);
            mv(e.save, bx + btn_w + px(8.0), y, btn_w, btn_h);
            mv(e.delete, bx + (btn_w + px(8.0)) * 2, y, btn_w, btn_h);
        });
    }
}

fn get_text(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h);
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..got as usize])
    }
}

fn set_text(h: HWND, s: &str) {
    let t = wide(s);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
    }
}

fn checked(h: HWND) -> bool {
    unsafe { SendMessageW(h, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize }
}

fn set_checked(h: HWND, on: bool) {
    let v = if on { BST_CHECKED } else { BST_UNCHECKED };
    unsafe {
        SendMessageW(h, BM_SETCHECK, Some(WPARAM(v.0 as usize)), None);
    }
}

fn preview(s: &str) -> String {
    let one_line = s.replace('\n', " ⏎ ");
    if one_line.chars().count() > 120 { one_line.chars().take(117).collect::<String>() + "..." } else { one_line }
}

/// Re-read the files into the list, optionally selecting `select`.
fn reload_rows(select: Option<&str>) {
    let mut rows: Vec<Row> = config::load_replacements()
        .into_iter()
        .map(|(trigger, text)| Row { trigger, text: Some(text) })
        .chain(config::load_secrets_raw().into_keys().map(|trigger| Row { trigger, text: None }))
        .collect();
    rows.sort_by_key(|r| r.trigger.to_lowercase());
    // Don't hold the editor state while talking to the list: it calls back into proc_ synchronously.
    let Some(list) = ED.with_borrow(|e| e.as_ref().map(|e| e.list)) else { return };
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        for (i, r) in rows.iter().enumerate() {
            let cells = [
                r.trigger.clone(),
                r.text.as_deref().map(preview).unwrap_or_else(|| "••••••••".into()),
                if r.text.is_some() { "Text".into() } else { "Secret".into() },
            ];
            for (col, cell) in cells.iter().enumerate() {
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
    ED.with_borrow_mut(|e| {
        if let Some(e) = e {
            e.rows = rows;
        }
    });
    if let Some(sel) = select {
        let idx = ED.with_borrow(|e| e.as_ref().and_then(|e| e.rows.iter().position(|r| r.trigger == sel)));
        if let Some(i) = idx {
            select_row(i as i32);
        }
    }
}

fn select_row(i: i32) {
    let Some(list) = ED.with_borrow(|e| e.as_ref().map(|e| e.list)) else { return };
    let mut item = LVITEMW { stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0), ..Default::default() };
    unsafe {
        // Clear, then set. Selecting fires LVN_ITEMCHANGED, which fills the form.
        SendMessageW(list, LVM_SETITEMSTATE, Some(WPARAM(usize::MAX)), Some(LPARAM(&item as *const _ as isize)));
        if i >= 0 {
            item.state = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
            SendMessageW(list, LVM_SETITEMSTATE, Some(WPARAM(i as usize)), Some(LPARAM(&item as *const _ as isize)));
            SendMessageW(list, LVM_ENSUREVISIBLE, Some(WPARAM(i as usize)), Some(LPARAM(0)));
        }
    }
}

fn update_buttons() {
    ED.with_borrow(|e| {
        let Some(e) = e else { return };
        unsafe {
            let _ = EnableWindow(e.delete, e.editing.is_some());
        }
        let secret_existing =
            e.editing.as_ref().is_some_and(|t| e.rows.iter().any(|r| &r.trigger == t && r.text.is_none()));
        set_text(e.hint, if secret_existing && checked(e.secret) { HINT_SECRET } else { HINT_DEFAULT });
    });
}

fn on_selected(i: usize) {
    let row = ED.with_borrow(|e| e.as_ref().and_then(|e| e.rows.get(i).map(|r| (r.trigger.clone(), r.text.clone()))));
    let Some((trigger, text)) = row else { return };
    ED.with_borrow_mut(|e| {
        let Some(e) = e else { return };
        set_text(e.trigger, &trigger);
        set_text(e.text, &text.clone().unwrap_or_default().replace('\n', "\r\n"));
        set_checked(e.secret, text.is_none());
        e.editing = Some(trigger);
    });
    update_buttons();
}

fn clear_form() {
    ED.with_borrow_mut(|e| {
        let Some(e) = e else { return };
        set_text(e.trigger, "");
        set_text(e.text, "");
        set_checked(e.secret, false);
        e.editing = None;
        unsafe {
            let _ = SetFocus(Some(e.trigger));
        }
    });
    select_row(-1);
    update_buttons();
}

fn alert(h: HWND, text: &str) {
    let t = wide(text);
    unsafe {
        MessageBoxW(Some(h), PCWSTR(t.as_ptr()), w!("qalam"), MB_OK | MB_ICONINFORMATION);
    }
}

fn confirm(h: HWND, text: &str) -> bool {
    let t = wide(text);
    unsafe { MessageBoxW(Some(h), PCWSTR(t.as_ptr()), w!("qalam"), MB_YESNO | MB_ICONQUESTION) == IDYES }
}

fn after_change(select: Option<&str>) {
    if let Err(e) = reload_config() {
        if let Some(h) = hwnd() {
            alert(h, &e);
        }
    }
    reload_rows(select);
}

fn save() {
    let Some((h, trigger_w, text_w, secret_w, editing)) =
        ED.with_borrow(|e| e.as_ref().map(|e| (e.hwnd, e.trigger, e.text, e.secret, e.editing.clone())))
    else {
        return;
    };
    let trigger = get_text(trigger_w).trim().to_string();
    let text = get_text(text_w).replace("\r\n", "\n");
    let secret = checked(secret_w);
    if trigger.is_empty() {
        return alert(h, "Type a trigger first, for example @@addr.");
    }
    if trigger.chars().any(char::is_whitespace) {
        return alert(h, "Triggers can't contain spaces: the replacement fires when you press space.");
    }

    let mut plain = config::load_replacements();
    let mut secrets = config::load_secrets_raw();
    let renamed_or_new = editing.as_deref() != Some(trigger.as_str());
    if renamed_or_new
        && (plain.contains_key(&trigger) || secrets.contains_key(&trigger))
        && !confirm(h, &format!("'{trigger}' already exists. Replace it?"))
    {
        return;
    }

    let value = if secret {
        if text.is_empty() {
            match editing.as_ref().and_then(|o| secrets.get(o).cloned()) {
                Some(blob) => blob, // keep the current secret
                None => return alert(h, "Type the secret text."),
            }
        } else {
            match dpapi::encrypt_b64(&text) {
                Some(b) => b,
                None => return alert(h, "Couldn't encrypt the secret."),
            }
        }
    } else if text.is_empty() {
        return alert(h, "Type the text to replace it with.");
    } else {
        text
    };

    if let Some(old) = &editing {
        plain.remove(old);
        secrets.remove(old);
    }
    plain.remove(&trigger);
    secrets.remove(&trigger);
    if secret {
        secrets.insert(trigger.clone(), value);
    } else {
        plain.insert(trigger.clone(), value);
    }
    if let Err(e) = config::save_replacements(&plain).and_then(|_| config::save_secrets_raw(&secrets)) {
        return alert(h, &format!("Couldn't save: {e}"));
    }
    ED.with_borrow_mut(|e| {
        if let Some(e) = e {
            e.editing = Some(trigger.clone());
            if secret {
                set_text(e.text, ""); // never leave a secret sitting in the box
            }
        }
    });
    after_change(Some(&trigger));
    update_buttons();
}

fn delete() {
    let Some((h, Some(trigger))) = ED.with_borrow(|e| e.as_ref().map(|e| (e.hwnd, e.editing.clone()))) else { return };
    if !confirm(h, &format!("Delete '{trigger}'?")) {
        return;
    }
    let mut plain = config::load_replacements();
    let mut secrets = config::load_secrets_raw();
    plain.remove(&trigger);
    secrets.remove(&trigger);
    if let Err(e) = config::save_replacements(&plain).and_then(|_| config::save_secrets_raw(&secrets)) {
        return alert(h, &format!("Couldn't save: {e}"));
    }
    after_change(None);
    clear_form();
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
                mmi.ptMinTrackSize = POINT { x: (560.0 * s) as i32, y: (420.0 * s) as i32 };
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(h, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER);
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as i32;
                let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
                match id {
                    ID_NEW => clear_form(),
                    ID_SAVE => save(),
                    ID_DELETE => delete(),
                    ID_SECRET if code == BN_CLICKED => update_buttons(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_NOTIFY => {
                let hdr = &*(lparam.0 as *const NMHDR);
                if hdr.idFrom == ID_LIST as usize && hdr.code == LVN_ITEMCHANGED {
                    let nm = &*(lparam.0 as *const NMLISTVIEW);
                    let now = nm.uNewState & LVIS_SELECTED.0 != 0;
                    let before = nm.uOldState & LVIS_SELECTED.0 != 0;
                    if now && !before && nm.iItem >= 0 {
                        on_selected(nm.iItem as usize);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(h);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(e) = ED.take() {
                    let _ = DeleteObject(e.font.into());
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, wparam, lparam),
        }
    }
}

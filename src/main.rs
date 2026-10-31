#![windows_subsystem = "windows"]
//! qalam: system-wide word suggestions, autocorrect and text replacement.
//! Threads: `hooks` (keyboard/mouse hooks + engine), `sender` (SendInput), main (popup, tray, focus tracking).

mod caret;
mod editor;
mod hook;
mod icon;
mod install;
mod overlay;
mod send;
mod spell;

use install::autostart;

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::SystemTime;
use qalam::config;
use qalam::dict::{Lang, Learned};
use qalam::engine::{Engine, Out};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CreateMutexW, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{AccessibleObjectFromEvent, HWINEVENTHOOK, IAccessible, SetWinEventHook};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

pub const WM_APP_POPUP: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
pub const WM_APP_FLASH: u32 = WM_APP + 3;
pub const WM_APP_SQUIGGLES: u32 = WM_APP + 4;
pub const WM_APP_SPELLMENU: u32 = WM_APP + 5;
const TIMER_RELOAD: usize = 1;
const TIMER_FLASH: usize = 2;
const FLASH_HOLD_MS: f32 = 250.0;
const FLASH_FADE_MS: f32 = 280.0;
const STATE_SYSTEM_PROTECTED: i32 = 0x2000_0000;

const MENU_PAUSE: usize = 10;
const MENU_CONFIG: usize = 11;
const MENU_FOLDER: usize = 12;
pub const MENU_QUIT: usize = 13;
const MENU_DEBUG: usize = 14;
const MENU_AUTOSTART: usize = 15;
pub const MENU_EDITOR: usize = 16;
const MENU_INSTALL: usize = 17;
const MENU_LANG_EN: usize = 20;
const MENU_LANG_AR: usize = 21;
const MENU_ARABIZI: usize = 22;
const MENU_KBMODE: usize = 23;
const MENU_HIGHLIGHT: usize = 24;
const MENU_UNDERLINE: usize = 25;
const MENU_AR_AUTOCORRECT: usize = 26;

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

static ICON: AtomicIsize = AtomicIsize::new(0);

/// The app icon (drawn once at startup).
pub fn app_icon() -> HICON {
    let h = ICON.load(Ordering::Relaxed);
    if h != 0 {
        return HICON(h as _);
    }
    let size = unsafe { GetSystemMetrics(SM_CXICON) }.max(32);
    let icon = icon::make(size);
    ICON.store(icon.0 as isize, Ordering::Relaxed);
    icon
}

pub struct Shared {
    pub engine: Mutex<Engine>,
    pub sender: Mutex<Sender<Vec<Out>>>,
    pub popup_hwnd: AtomicIsize,
    /// Screen rects of the suggestions currently shown, for mouse clicks.
    pub popup_items: Mutex<Vec<RECT>>,
    pub password: AtomicBool,
    pub disabled: AtomicBool,
    pub paused: AtomicBool,
    pub newline_enter: AtomicBool,
    pub spell_tx: Mutex<Sender<spell::Job>>,
    pub squiggles: Mutex<spell::Squiggles>,
    /// Screen rect of the word to flash after an autocorrect.
    pub flash_rect: Mutex<Option<RECT>>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

// ---------- debug log (never records typed characters) ----------

static DEBUG: AtomicBool = AtomicBool::new(false);
static LOG: Mutex<Option<(std::fs::File, std::time::Instant)>> = Mutex::new(None);

pub fn debug_on() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

pub fn dlog(f: impl FnOnce() -> String) {
    if !debug_on() {
        return;
    }
    use std::io::Write;
    if let Some((file, start)) = lock(&LOG).as_mut() {
        let _ = writeln!(file, "{:>8.3} [{}] {}", start.elapsed().as_secs_f64(), std::thread::current().name().unwrap_or("main"), f());
    }
}

fn set_debug(on: bool) {
    let mut g = lock(&LOG);
    if on {
        let _ = std::fs::create_dir_all(config::dir());
        *g = std::fs::File::create(config::dir().join("debug.log")).ok().map(|f| (f, std::time::Instant::now()));
    } else {
        *g = None;
    }
    DEBUG.store(on && g.is_some(), Ordering::Relaxed);
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn shared() -> &'static Shared {
    SHARED.get().expect("shared state")
}

pub fn shared_ready() -> bool {
    SHARED.get().is_some()
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn message_box(text: &str) {
    let t = wide(text);
    unsafe { MessageBoxW(None, PCWSTR(t.as_ptr()), w!("qalam"), MB_OK | MB_ICONWARNING) };
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Take the single-instance lock. With `wait`, keep trying for a few seconds (an old copy may be exiting).
fn single_instance(wait: bool) -> bool {
    for _ in 0..if wait { 50 } else { 1 } {
        unsafe {
            if let Ok(h) = CreateMutexW(None, true, w!("Local\\qalam-single-instance")) {
                if GetLastError() != ERROR_ALREADY_EXISTS {
                    let _ = h; // never closed: held until the process exits
                    return true;
                }
                let _ = CloseHandle(h);
            }
        }
        if wait {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    false
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |a: &str| args.iter().any(|x| x.eq_ignore_ascii_case(a));
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    if has("--uninstall") {
        install::uninstall();
        return;
    }
    // Earlier builds were called "typeahead": bring their settings over and drop their startup entry.
    config::migrate_old_name();
    autostart::remove_legacy();
    let after_install = has("--after-install");
    if !single_instance(after_install) {
        // Already running: launching again opens the Replacements window there.
        unsafe {
            if let Ok(h) = FindWindowW(w!("qalam_tray"), None) {
                let _ = PostMessageW(Some(h), WM_COMMAND, WPARAM(MENU_EDITOR), LPARAM(0));
            }
        }
        return;
    }
    if !after_install && !install::is_installed_copy() && !config::portable() {
        match install::first_run_prompt() {
            install::FirstRun::Install => match install::install() {
                Ok(exe) => {
                    install::launch_installed(&exe);
                    return;
                }
                Err(e) => message_box(&format!("Install failed: {e}\n\nRunning from here instead.")),
            },
            install::FirstRun::Portable | install::FirstRun::JustRun => {}
            install::FirstRun::Quit => return,
        }
    }
    config::migrate_replacements();

    let cfg = config::load().unwrap_or_else(|e| {
        message_box(&format!("{e}\n\nUsing default settings until it's fixed."));
        config::Config::default()
    });
    let learned = std::fs::read_to_string(config::learned_path()).map(|t| Learned::parse(&t)).unwrap_or_default();
    let newline_enter = cfg.general.newline.eq_ignore_ascii_case("enter");
    let engine = Engine::new(learned, cfg, config::extra_replacements());

    let _ = SHARED.set(Shared {
        engine: Mutex::new(engine),
        sender: Mutex::new(send::spawn()),
        popup_hwnd: AtomicIsize::new(0),
        popup_items: Mutex::new(Vec::new()),
        password: AtomicBool::new(false),
        disabled: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        newline_enter: AtomicBool::new(newline_enter),
        spell_tx: Mutex::new(spell::spawn()),
        squiggles: Mutex::new(spell::Squiggles::default()),
        flash_rect: Mutex::new(None),
    });
    if std::env::var_os("QALAM_DEBUG").is_some() {
        set_debug(true);
    }

    unsafe { ui_main(after_install) };
}

// ---------- UI thread ----------

struct Ui {
    cfg_mtime: Option<SystemTime>,
    secrets_mtime: Option<SystemTime>,
    repl_mtime: Option<SystemTime>,
    ticks: u32,
    last_focus: (isize, i32, i32),
}

thread_local! {
    static UI: std::cell::RefCell<Option<Ui>> = const { std::cell::RefCell::new(None) };
    static FONT: std::cell::Cell<(u32, isize)> = const { std::cell::Cell::new((0, 0)) };
}

unsafe fn ui_main(after_install: bool) {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();

        let popup_class = w!("qalam_popup");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(popup_proc),
            hInstance: hinst,
            lpszClassName: popup_class,
            hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let tray_class = w!("qalam_tray");
        let wc2 = WNDCLASSW { lpfnWndProc: Some(tray_proc), hInstance: hinst, lpszClassName: tray_class, ..Default::default() };
        RegisterClassW(&wc2);

        let popup = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            popup_class,
            w!("qalam"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hinst),
            None,
        )
        .expect("popup window");
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            popup,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const _,
            size_of_val(&pref) as u32,
        );
        let tray = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            tray_class,
            w!("qalam tray"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )
        .expect("tray window");

        shared().popup_hwnd.store(popup.0 as isize, Ordering::Relaxed);
        TRAY_HWND.store(tray.0 as isize, Ordering::Relaxed);
        FLASH_WIN.set(Some(overlay::Layered::new()));
        SQUIG_WIN.set(Some(overlay::Layered::new()));
        UI.set(Some(Ui {
            cfg_mtime: mtime(&config::config_path()),
            secrets_mtime: mtime(&config::secrets_path()),
            repl_mtime: mtime(&config::replacements_path()),
            ticks: 0,
            last_focus: (0, 0, 0),
        }));

        tray_icon(tray, NIM_ADD, None);
        SetTimer(Some(tray), TIMER_RELOAD, 1500, None);
        SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT);
        SetWinEventHook(EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        on_foreground(GetForegroundWindow());

        hook::spawn();
        if after_install {
            tray_icon(
                tray,
                NIM_MODIFY,
                Some("Installed. qalam starts with Windows; click the tray icon to add replacements."),
            );
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if let Some(ed) = editor::hwnd()
                && IsDialogMessageW(ed, &msg).as_bool()
            {
                continue; // Tab between fields
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        save_learned(true);
        tray_icon(tray, NIM_DELETE, None);
    }
}

unsafe fn tray_icon(tray: HWND, op: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE, info: Option<&str>) {
    unsafe {
        let mut nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: tray,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: app_icon(),
            ..Default::default()
        };
        let paused = shared().paused.load(Ordering::Relaxed);
        let tip = wide(if paused { "qalam (paused)" } else { "qalam" });
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        if let Some(text) = info {
            nid.uFlags |= NIF_INFO;
            nid.dwInfoFlags = NIIF_INFO;
            let t = wide(&text.chars().take(250).collect::<String>());
            nid.szInfo[..t.len()].copy_from_slice(&t);
            let title = wide("qalam");
            nid.szInfoTitle[..title.len()].copy_from_slice(&title);
        }
        let _ = Shell_NotifyIconW(op, &nid);
    }
}

fn save_learned(force: bool) {
    let text = {
        let mut eng = lock(&shared().engine);
        if !eng.learned.dirty && !force {
            return;
        }
        eng.learned.dirty = false;
        eng.learned.serialize()
    };
    let _ = std::fs::create_dir_all(config::dir());
    let _ = std::fs::write(config::learned_path(), text);
}

pub fn reload_config() -> Result<(), String> {
    let cfg = config::load()?;
    shared().newline_enter.store(cfg.general.newline.eq_ignore_ascii_case("enter"), Ordering::Relaxed);
    lock(&shared().engine).set_config(cfg, config::extra_replacements());
    refresh_popup();
    Ok(())
}

fn refresh_popup() {
    let h = shared().popup_hwnd.load(Ordering::Relaxed);
    unsafe {
        let _ = PostMessageW(Some(HWND(h as _)), WM_APP_POPUP, WPARAM(0), LPARAM(0));
    }
}

fn process_name(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    process_name_pid(pid)
}

pub fn process_name_pid(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }
}

fn on_foreground(hwnd: HWND) {
    let s = shared();
    let name = process_name(hwnd).unwrap_or_default();
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let ours = pid == std::process::id();
    let (disabled, epoch) = {
        let mut eng = lock(&s.engine);
        eng.reset(false);
        (ours || eng.cfg.general.disabled_apps.iter().any(|a| a.eq_ignore_ascii_case(&name)), eng.epoch)
    };
    s.disabled.store(disabled, Ordering::Relaxed);
    s.password.store(false, Ordering::Relaxed);
    if !ours {
        hook::spell(spell::Job::Dirty { hide: true });
        hook::spell(spell::Job::Sync { epoch });
    }
    dlog(|| format!("foreground {name} class={} disabled={disabled} -> reset", class_name(hwnd)));
    refresh_popup();
}

unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    unsafe {
        if event == EVENT_SYSTEM_FOREGROUND {
            on_foreground(hwnd);
            return;
        }
        if event == EVENT_OBJECT_LOCATIONCHANGE {
            // The foreground window moved or resized: squiggles are in the wrong place until the next scan.
            if id_object == OBJID_WINDOW.0 && hwnd == GetForegroundWindow() {
                hook::spell(spell::Job::Dirty { hide: true });
            }
            return;
        }
        // EVENT_OBJECT_FOCUS: new text box? Some apps re-fire focus for the same element while typing; ignore those.
        let key = (hwnd.0 as isize, id_object, id_child);
        let same = UI.with_borrow_mut(|ui| {
            let ui = ui.as_mut().unwrap();
            let same = ui.last_focus == key;
            ui.last_focus = key;
            same
        });
        if same {
            dlog(|| format!("focus (same) hwnd={:x} class={} obj={id_object} child={id_child} -> ignored", key.0, class_name(hwnd)));
            return;
        }
        let mut password = false;
        let mut acc: Option<IAccessible> = None;
        let mut child = VARIANT::default();
        if AccessibleObjectFromEvent(hwnd, id_object as u32, id_child as u32, &mut acc, &mut child).is_ok()
            && let Some(acc) = acc
            && let Ok(state) = acc.get_accState(&child)
        {
            password = i32::try_from(&state).is_ok_and(|s| s & STATE_SYSTEM_PROTECTED != 0);
        }
        let s = shared();
        s.password.store(password, Ordering::Relaxed);
        dlog(|| format!("focus hwnd={:x} class={} obj={id_object} child={id_child} password={password} -> reset", key.0, class_name(hwnd)));
        let epoch = {
            let mut eng = lock(&s.engine);
            eng.reset(false);
            eng.epoch
        };
        refresh_popup();
        hook::spell(spell::Job::Dirty { hide: true });
        hook::spell(spell::Job::Sync { epoch });
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER if wparam.0 == TIMER_RELOAD => {
                let (cm, sm, rm) =
                    (mtime(&config::config_path()), mtime(&config::secrets_path()), mtime(&config::replacements_path()));
                let changed = UI.with_borrow_mut(|ui| {
                    let ui = ui.as_mut().unwrap();
                    ui.ticks += 1;
                    let c = ui.cfg_mtime != cm || ui.secrets_mtime != sm || ui.repl_mtime != rm;
                    ui.cfg_mtime = cm;
                    ui.secrets_mtime = sm;
                    ui.repl_mtime = rm;
                    c
                });
                if changed && let Err(e) = reload_config() {
                    tray_icon(hwnd, NIM_MODIFY, Some(&e));
                }
                if UI.with_borrow(|ui| ui.as_ref().unwrap().ticks % 20 == 0) {
                    save_learned(false);
                }
                LRESULT(0)
            }
            WM_APP_TRAY => {
                let ev = (lparam.0 & 0xFFFF) as u32;
                if ev == WM_LBUTTONUP {
                    editor::open();
                } else if ev == WM_RBUTTONUP {
                    show_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                match wparam.0 & 0xFFFF {
                    MENU_PAUSE => {
                        let s = shared();
                        let p = !s.paused.load(Ordering::Relaxed);
                        s.paused.store(p, Ordering::Relaxed);
                        lock(&s.engine).reset(false);
                        refresh_popup();
                        tray_icon(hwnd, NIM_MODIFY, None);
                    }
                    MENU_CONFIG => {
                        let p = wide(&config::config_path().to_string_lossy());
                        ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
                    }
                    MENU_FOLDER => {
                        let p = wide(&config::dir().to_string_lossy());
                        ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
                    }
                    MENU_DEBUG => set_debug(!debug_on()),
                    MENU_AUTOSTART => {
                        if !autostart::set(!autostart::enabled()) {
                            tray_icon(hwnd, NIM_MODIFY, Some("Couldn't change the Start with Windows setting."));
                        }
                    }
                    MENU_EDITOR => editor::open(),
                    MENU_LANG_EN => toggle_lang(hwnd, "en"),
                    MENU_LANG_AR => toggle_lang(hwnd, "ar"),
                    MENU_AR_AUTOCORRECT => toggle_list(hwnd, "autocorrect_languages", "ar"),
                    MENU_ARABIZI => toggle_bool(hwnd, "arabizi", |g| g.arabizi),
                    MENU_HIGHLIGHT => toggle_bool(hwnd, "highlight_corrections", |g| g.highlight_corrections),
                    MENU_UNDERLINE => toggle_bool(hwnd, "underline_misspelled", |g| g.underline_misspelled),
                    MENU_KBMODE => {
                        let kb = lock(&shared().engine).cfg.general.language_mode.eq_ignore_ascii_case("keyboard");
                        apply_setting(hwnd, "language_mode", if kb { "\"all\"" } else { "\"keyboard\"" });
                    }
                    MENU_INSTALL => match install::install() {
                        Ok(exe) => {
                            install::launch_installed(&exe);
                            PostQuitMessage(0);
                        }
                        Err(e) => tray_icon(hwnd, NIM_MODIFY, Some(&format!("Install failed: {e}"))),
                    },
                    MENU_QUIT => PostQuitMessage(0),
                    _ => {}
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn apply_setting(tray: HWND, key: &str, value: &str) {
    let r = config::set_general(key, value).map_err(|e| e.to_string()).and_then(|_| reload_config());
    if let Err(e) = r {
        unsafe { tray_icon(tray, NIM_MODIFY, Some(&format!("Couldn't save setting: {e}"))) };
    }
}

fn toggle_bool(tray: HWND, key: &str, get: impl Fn(&config::General) -> bool) {
    let on = get(&lock(&shared().engine).cfg.general);
    apply_setting(tray, key, if on { "false" } else { "true" });
}

fn toml_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(", "))
}

fn toggle_list(tray: HWND, key: &str, code: &str) {
    let mut v = {
        let g = &lock(&shared().engine).cfg.general;
        if key == "languages" { g.languages.clone() } else { g.autocorrect_languages.clone() }
    };
    if let Some(i) = v.iter().position(|x| x.eq_ignore_ascii_case(code)) {
        v.remove(i);
    } else {
        v.push(code.to_string());
    }
    apply_setting(tray, key, &toml_list(&v));
}

fn toggle_lang(tray: HWND, code: &str) {
    let langs = lock(&shared().engine).enabled_langs();
    let on = langs.iter().any(|l| l.code() == code);
    if on && langs.len() == 1 {
        return; // keep at least one language
    }
    toggle_list(tray, "languages", code);
}

unsafe fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let paused = shared().paused.load(Ordering::Relaxed);
        let (langs, g) = {
            let e = lock(&shared().engine);
            (e.enabled_langs(), e.cfg.general.clone())
        };
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let ar_ac = g.autocorrect_languages.iter().any(|l| l.eq_ignore_ascii_case("ar"));
        let kb = g.language_mode.eq_ignore_ascii_case("keyboard");
        if let Ok(sub) = CreatePopupMenu() {
            let _ = AppendMenuW(sub, check(langs.contains(&Lang::En)), MENU_LANG_EN, w!("English"));
            let _ = AppendMenuW(sub, check(langs.contains(&Lang::Ar)), MENU_LANG_AR, w!("Arabic (العربية)"));
            let _ = AppendMenuW(sub, check(ar_ac), MENU_AR_AUTOCORRECT, w!("Autocorrect Arabic"));
            let _ = AppendMenuW(sub, check(g.arabizi), MENU_ARABIZI, w!("Arabizi → Arabic suggestions"));
            let _ = AppendMenuW(sub, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(sub, check(kb), MENU_KBMODE, w!("Only the keyboard layout's language"));
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Languages"));
        }
        let _ = AppendMenuW(menu, check(g.highlight_corrections), MENU_HIGHLIGHT, w!("Highlight corrections"));
        let _ = AppendMenuW(menu, check(g.underline_misspelled), MENU_UNDERLINE, w!("Underline misspelled words"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = InsertMenuW(menu, 0, MF_BYPOSITION | MF_STRING, MENU_EDITOR, w!("Replacements..."));
        let _ = InsertMenuW(menu, 1, MF_BYPOSITION | MF_SEPARATOR, 0, None);
        let _ = SetMenuDefaultItem(menu, MENU_EDITOR as u32, 0);
        let _ = AppendMenuW(menu, if paused { MF_CHECKED } else { MF_UNCHECKED }, MENU_PAUSE, w!("Paused"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_CONFIG, w!("Edit settings file"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_FOLDER, w!("Open data folder"));
        let _ = AppendMenuW(menu, if autostart::enabled() { MF_CHECKED } else { MF_UNCHECKED }, MENU_AUTOSTART, w!("Start with Windows"));
        let _ = AppendMenuW(menu, if debug_on() { MF_CHECKED } else { MF_UNCHECKED }, MENU_DEBUG, w!("Debug log (no typed text)"));
        if !install::is_installed_copy() && !config::portable() {
            let _ = AppendMenuW(menu, MF_STRING, MENU_INSTALL, w!("Install for this user..."));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, p.x, p.y, Some(0), hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

// ---------- popup ----------

struct PopupView {
    labels: Vec<String>,
    selected: usize,
    nav: bool,
}

thread_local! {
    static VIEW: std::cell::RefCell<PopupView> = const { std::cell::RefCell::new(PopupView { labels: Vec::new(), selected: 0, nav: false }) };
    /// Item rects in client coords, for painting.
    static LAYOUT: std::cell::RefCell<Vec<RECT>> = const { std::cell::RefCell::new(Vec::new()) };
    static SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

unsafe fn font_for(dpi: u32) -> HFONT {
    unsafe {
        let (d, f) = FONT.get();
        if d == dpi && f != 0 {
            return HFONT(f as _);
        }
        if f != 0 {
            let _ = DeleteObject(HGDIOBJ(f as _));
        }
        let height = -((15 * dpi as i32) / 96);
        let font = CreateFontW(
            height,
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
        FONT.set((dpi, font.0 as isize));
        font
    }
}

unsafe fn update_popup(hwnd: HWND) {
    unsafe {
        let view = {
            let eng = lock(&shared().engine);
            PopupView {
                labels: eng.suggestions.iter().map(|s| s.label.clone()).collect(),
                selected: eng.selected,
                nav: eng.nav,
            }
        };
        let s = shared();
        if view.labels.is_empty() || s.paused.load(Ordering::Relaxed) || s.disabled.load(Ordering::Relaxed) {
            lock(&s.popup_items).clear();
            let _ = ShowWindow(hwnd, SW_HIDE);
            dlog(|| "popup hidden".into());
            return;
        }

        let (caret, source) = caret::caret_rect();
        let anchor = POINT { x: caret.left, y: caret.bottom };
        let mon = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let scale = dx as f32 / 96.0;
        SCALE.set(scale);
        let px = |v: f32| (v * scale).round() as i32;

        // Measure.
        let hdc = GetDC(Some(hwnd));
        let old = SelectObject(hdc, HGDIOBJ(font_for(dx).0));
        let pad_x = px(14.0);
        let pad_y = px(7.0);
        let outer = px(4.0);
        let mut x = outer;
        let mut rects = Vec::new();
        let mut text_h = 0;
        for l in &view.labels {
            let wl: Vec<u16> = l.encode_utf16().collect();
            let mut sz = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &wl, &mut sz);
            text_h = text_h.max(sz.cy);
            let w = sz.cx + pad_x * 2;
            rects.push(RECT { left: x, top: outer, right: x + w, bottom: 0 });
            x += w + px(2.0);
        }
        SelectObject(hdc, old);
        ReleaseDC(Some(hwnd), hdc);
        let item_h = text_h + pad_y * 2;
        for r in &mut rects {
            r.bottom = outer + item_h;
        }
        let width = x - px(2.0) + outer;
        let height = item_h + outer * 2;

        // Place under the caret, flipped above it near the bottom of the screen.
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let work = mi.rcWork;
        let mut left = caret.left - outer;
        let mut top = caret.bottom + px(6.0);
        if top + height > work.bottom {
            top = caret.top - height - px(6.0);
        }
        left = left.clamp(work.left, (work.right - width).max(work.left));

        {
            let mut items = lock(&s.popup_items);
            *items = rects.iter().map(|r| RECT { left: r.left + left, top: r.top + top, right: r.right + left, bottom: r.bottom + top }).collect();
        }
        dlog(|| {
            format!(
                "popup shown n={} caret[{source}]=({},{} {}x{}) at ({left},{top}) {width}x{height}",
                view.labels.len(),
                caret.left,
                caret.top,
                caret.right - caret.left,
                caret.bottom - caret.top
            )
        });
        LAYOUT.set(rects);
        VIEW.set(view);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), left, top, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let bg = CreateSolidBrush(rgb(0x2b, 0x2b, 0x2b));
        FillRect(hdc, &client, bg);
        let _ = DeleteObject(bg.into());

        let scale = SCALE.get();
        let old = SelectObject(hdc, HGDIOBJ(font_for((scale * 96.0).round() as u32).0));
        SetBkMode(hdc, TRANSPARENT);
        VIEW.with_borrow(|v| {
            LAYOUT.with_borrow(|rects| {
                for (i, (label, r)) in v.labels.iter().zip(rects).enumerate() {
                    let highlighted = i == v.selected;
                    if highlighted {
                        let c = if v.nav { rgb(0x4a, 0x4a, 0x55) } else { rgb(0x3a, 0x3a, 0x3a) };
                        let b = CreateSolidBrush(c);
                        let rad = (6.0 * scale) as i32;
                        let old_b = SelectObject(hdc, b.into());
                        let old_p = SelectObject(hdc, GetStockObject(NULL_PEN));
                        let _ = RoundRect(hdc, r.left, r.top, r.right, r.bottom, rad, rad);
                        SelectObject(hdc, old_p);
                        SelectObject(hdc, old_b);
                        let _ = DeleteObject(b.into());
                    }
                    SetTextColor(hdc, if highlighted { rgb(0xff, 0xff, 0xff) } else { rgb(0xd0, 0xd0, 0xd0) });
                    let mut text: Vec<u16> = label.encode_utf16().collect();
                    let mut rr = *r;
                    let rtl = if label.chars().any(qalam::dict::is_arabic) { DT_RTLREADING } else { DRAW_TEXT_FORMAT(0) };
                    DrawTextW(hdc, &mut text, &mut rr, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | rtl);
                }
            })
        });
        SelectObject(hdc, old);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_POPUP => {
                update_popup(hwnd);
                LRESULT(0)
            }
            WM_APP_FLASH => {
                start_flash(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_FLASH => {
                step_flash(hwnd);
                LRESULT(0)
            }
            WM_APP_SQUIGGLES => {
                draw_squiggles();
                LRESULT(0)
            }
            WM_APP_SPELLMENU => {
                spell_menu(wparam.0, lparam.0 as u64);
                LRESULT(0)
            }
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

// ---------- correction flash, squiggles, spelling menu ----------

thread_local! {
    static FLASH_WIN: std::cell::RefCell<Option<overlay::Layered>> = const { std::cell::RefCell::new(None) };
    static SQUIG_WIN: std::cell::RefCell<Option<overlay::Layered>> = const { std::cell::RefCell::new(None) };
    static FLASH: std::cell::RefCell<Option<(i32, i32, overlay::Canvas, std::time::Instant)>> = const { std::cell::RefCell::new(None) };
}

const ACCENT: (u8, u8, u8) = (0x3D, 0x7A, 0xF0);
const SQUIGGLE_RED: (u8, u8, u8) = (0xE8, 0x11, 0x23);

fn dpi_scale_at(p: POINT) -> f32 {
    unsafe {
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        dx as f32 / 96.0
    }
}

unsafe fn start_flash(popup: HWND) {
    unsafe {
        let Some(r) = lock(&shared().flash_rect).take() else { return };
        let scale = dpi_scale_at(POINT { x: r.left, y: r.top });
        let pad = (2.0 * scale).round();
        let (w, h) = ((r.right - r.left) as f32 + pad * 2.0, (r.bottom - r.top) as f32 + pad * 2.0);
        let mut c = overlay::Canvas::new(w.ceil() as i32, h.ceil() as i32);
        c.round_rect(0.0, 0.0, w, h, 4.0 * scale, ACCENT, 0.30);
        c.round_rect(pad, h - pad - 2.0 * scale, w - pad, h - pad, scale, ACCENT, 0.95);
        let (x, y) = (r.left - pad as i32, r.top - pad as i32);
        FLASH_WIN.with_borrow(|f| {
            if let Some(f) = f {
                f.show(x, y, &c, 255)
            }
        });
        FLASH.set(Some((x, y, c, std::time::Instant::now())));
        SetTimer(Some(popup), TIMER_FLASH, 16, None);
    }
}

unsafe fn step_flash(popup: HWND) {
    unsafe {
        let done = FLASH.with_borrow(|f| {
            let Some((x, y, c, start)) = f else { return true };
            let t = start.elapsed().as_secs_f32() * 1000.0;
            if t < FLASH_HOLD_MS {
                return false;
            }
            let k = ((t - FLASH_HOLD_MS) / FLASH_FADE_MS).min(1.0);
            let ease = 1.0 - (1.0 - k).powi(3); // ease-out
            let alpha = (255.0 * (1.0 - ease)) as u8;
            FLASH_WIN.with_borrow(|w| {
                if let Some(w) = w {
                    if k >= 1.0 { w.hide() } else { w.show(*x, *y, c, alpha) }
                }
            });
            k >= 1.0
        });
        if done {
            let _ = KillTimer(Some(popup), TIMER_FLASH);
            FLASH.set(None);
        }
    }
}

fn draw_squiggles() {
    let items = lock(&shared().squiggles).items.clone();
    SQUIG_WIN.with_borrow(|w| {
        let Some(w) = w else { return };
        let rects: Vec<RECT> = items.iter().flat_map(|q| q.rects.iter().copied()).collect();
        if rects.is_empty() {
            return w.hide();
        }
        let scale = dpi_scale_at(POINT { x: rects[0].left, y: rects[0].top });
        let pad = (3.0 * scale).ceil() as i32;
        let ux = rects.iter().map(|r| r.left).min().unwrap_or(0);
        let uy = rects.iter().map(|r| r.bottom).min().unwrap_or(0) - pad * 2;
        let ur = rects.iter().map(|r| r.right).max().unwrap_or(0);
        let ub = rects.iter().map(|r| r.bottom).max().unwrap_or(0) + pad;
        // Huge areas mean something odd (offscreen document): skip rather than allocate a giant bitmap.
        if (ur - ux) > 8000 || (ub - uy) > 5000 {
            return w.hide();
        }
        let mut c = overlay::Canvas::new(ur - ux, ub - uy);
        for r in &rects {
            c.squiggle(r.left - ux, r.right - ux, (r.bottom - uy) as f32 - 1.0 * scale, scale, SQUIGGLE_RED);
        }
        w.show(ux, uy, &c, 255);
    });
}

unsafe fn spell_menu(id: usize, scan_id: u64) {
    unsafe {
        let word = {
            let s = lock(&shared().squiggles);
            if s.scan_id != scan_id {
                return;
            }
            match s.items.iter().find(|q| q.id == id) {
                Some(q) => q.word.clone(),
                None => return,
            }
        };
        let fixes = lock(&shared().engine).fixes_for(&word, 4);
        let tray = HWND(TRAY_HWND.load(Ordering::Relaxed) as _);
        let prev = GetForegroundWindow();
        let Ok(menu) = CreatePopupMenu() else { return };
        for (i, f) in fixes.iter().enumerate() {
            let t = wide(f);
            let _ = AppendMenuW(menu, MF_STRING, 1000 + i, PCWSTR(t.as_ptr()));
        }
        if fixes.is_empty() {
            let _ = AppendMenuW(menu, MF_GRAYED, 0, w!("No suggestions"));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let add = wide(&format!("Add \u{201c}{word}\u{201d} to dictionary"));
        let _ = AppendMenuW(menu, MF_STRING, 999, PCWSTR(add.as_ptr()));
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let _ = SetForegroundWindow(tray);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, Some(0), tray, None).0 as usize;
        let _ = PostMessageW(Some(tray), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let _ = SetForegroundWindow(prev);
        if (1000..1000 + fixes.len()).contains(&cmd) {
            hook::spell(spell::Job::Replace { scan_id, id, text: fixes[cmd - 1000].clone() });
        } else if cmd == 999 {
            lock(&shared().engine).learned.accept(&word);
            hook::spell(spell::Job::Dirty { hide: false });
        }
    }
}

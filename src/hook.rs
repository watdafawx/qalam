//! Low-level keyboard and mouse hooks, on their own thread so slow UI work never delays typing.
//! Windows silently removes a low-level hook that takes too long, so everything here stays quick.

use crate::spell::Job;
use crate::{WM_APP_POPUP, WM_APP_SPELLHOVER, WM_APP_SPELLMENU, dlog, lock, send::MAGIC, shared};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use qalam::dict::Lang;
use qalam::engine::{Action, Ctx, Key};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    /// Keys whose key-down we ate; eat the key-up too.
    static EATEN: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static EAT_MOUSE_UP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EAT_RMOUSE_UP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Rectangle of the underlined word the mouse is resting on.
    static HOVER_RECT: std::cell::Cell<Option<windows::Win32::Foundation::RECT>> = const { std::cell::Cell::new(None) };
    /// Language of the foreground keyboard layout, updated on every key.
    static KB_LANG: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) };
}

pub fn spell(job: Job) {
    let _ = lock(&shared().spell_tx).send(job);
}

pub fn spawn() {
    std::thread::Builder::new()
        .name("hooks".into())
        .spawn(|| unsafe {
            let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0).expect("keyboard hook");
            let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0).expect("mouse hook");
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(kb);
            let _ = UnhookWindowsHookEx(ms);
        })
        .expect("spawn hooks");
}

fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

fn notify_popup() {
    let h = shared().popup_hwnd.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_APP_POPUP, WPARAM(0), LPARAM(0));
        }
    }
}

fn active() -> bool {
    let s = shared();
    !s.paused.load(Ordering::Relaxed) && !s.disabled.load(Ordering::Relaxed) && !s.fullscreen.load(Ordering::Relaxed)
}

/// What the engine said, after forwarding any text to type.
fn run(what: &str, f: impl FnOnce(&mut qalam::engine::Engine, Ctx) -> Action) -> bool {
    let s = shared();
    // Password boxes and terminals both mean "replacements only".
    let restricted = s.password.load(Ordering::Relaxed) || s.replacements_only.load(Ordering::Relaxed);
    let ctx = Ctx { password: restricted, kb_lang: KB_LANG.get() };
    let (action, changed, flash, lost) = {
        let mut eng = lock(&s.engine);
        let before = eng.popup_version;
        let epoch_before = eng.epoch;
        let a = f(&mut eng, ctx);
        let flash = eng.flash.take().filter(|_| eng.cfg.general.highlight_corrections);
        dlog(|| {
            let act = match &a {
                Action::Pass => "pass".to_string(),
                Action::Swallow => "swallow".to_string(),
                Action::Send(o) => format!("send {} ops", o.len()),
            };
            format!("{what} pw={} -> {act} | {}", ctx.password, eng.debug_state())
        });
        let lost = (eng.epoch != epoch_before).then_some(eng.epoch);
        (a, eng.popup_version != before, flash, lost)
    };
    if let Some(epoch) = lost {
        spell(Job::Sync { epoch });
    }
    if changed {
        notify_popup();
    }
    if let Some((word, sep)) = flash {
        spell(Job::Flash { word, sep });
    }
    match action {
        Action::Pass => false,
        Action::Swallow => true,
        Action::Send(outs) => {
            let _ = lock(&s.sender).send(outs);
            true
        }
    }
}

fn reset(why: &str, dirty: bool) {
    run(&format!("reset({why}, dirty={dirty})"), |e, _| {
        e.reset(dirty);
        Action::Pass
    });
}

/// The text a key types (usually one character; the Arabic لا key types two).
fn translate(k: &KBDLLHOOKSTRUCT, shift: bool, altgr: bool) -> Result<Option<String>, ()> {
    unsafe {
        let fg = GetForegroundWindow();
        let tid = GetWindowThreadProcessId(fg, None);
        let hkl = GetKeyboardLayout(tid);
        KB_LANG.set(Lang::from_langid((hkl.0 as usize & 0x3FF) as u16));
        let mut state = [0u8; 256];
        if shift {
            state[VK_SHIFT.0 as usize] = 0x80;
        }
        if GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0 {
            state[VK_CAPITAL.0 as usize] = 0x01;
        }
        if altgr {
            state[VK_CONTROL.0 as usize] = 0x80;
            state[VK_MENU.0 as usize] = 0x80;
        }
        let mut buf = [0u16; 8];
        // Flag 0x4: don't touch the keyboard's dead-key state (Windows 10 1607+).
        let n = ToUnicodeEx(k.vkCode, k.scanCode, &state, &mut buf, 0x4, Some(hkl));
        match n {
            n if n < 0 => Err(()), // dead key
            0 => Ok(None),
            n => {
                let s: String = String::from_utf16_lossy(&buf[..n as usize]).chars().filter(|c| !c.is_control()).collect();
                Ok((!s.is_empty()).then_some(s))
            }
        }
    }
}

fn on_key_down(k: &KBDLLHOOKSTRUCT) -> bool {
    let vk = VIRTUAL_KEY(k.vkCode as u16);
    if matches!(
        vk,
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT | VK_CONTROL | VK_LCONTROL | VK_RCONTROL | VK_MENU | VK_LMENU | VK_RMENU
            | VK_CAPITAL | VK_NUMLOCK | VK_SCROLL
    ) {
        return false;
    }
    let ctrl = down(VK_CONTROL);
    let alt = down(VK_MENU);
    let shift = down(VK_SHIFT);
    let win = down(VK_LWIN) || down(VK_RWIN);
    let plain = !ctrl && !alt && !win;

    let key = match vk {
        VK_BACK if ctrl && !alt => Key::CtrlBackspace,
        VK_BACK if plain => Key::Backspace,
        VK_RETURN if plain => Key::Enter,
        VK_TAB if plain && !shift => Key::Tab,
        VK_ESCAPE if plain => Key::Escape,
        VK_UP if plain && !shift => Key::Up,
        VK_DOWN if plain && !shift => Key::Down,
        VK_LEFT if plain && !shift => Key::Left,
        VK_RIGHT if plain && !shift => Key::Right,
        VK_DELETE | VK_INSERT => return false,
        VK_LWIN | VK_RWIN | VK_APPS => return false,
        _ if (vk.0 >= VK_F1.0 && vk.0 <= VK_F24.0) || vk == VK_ESCAPE => return false,
        _ if win || (ctrl != alt) => {
            // Shortcut (Ctrl+V, Alt+Tab, Win+...): we no longer know what's left of the caret.
            let letter = (ctrl && !alt && !win && (0x41..=0x5A).contains(&vk.0))
                .then(|| (vk.0 as u8 as char).to_ascii_lowercase());
            run(&format!("shortcut ctrl+{}", letter.unwrap_or('?')), |e, _| {
                e.shortcut(letter);
                Action::Pass
            });
            return false;
        }
        _ => match translate(k, shift, ctrl && alt) {
            Ok(Some(s)) => {
                let mut cs: Vec<char> = s.chars().collect();
                let last = cs.pop().expect("non-empty");
                for c in cs {
                    run("key letter (multi)", |e, ctx| e.key(Key::Char(c), ctx));
                }
                Key::Char(last)
            }
            Ok(None) => {
                // Home/End/PgUp/PgDn, Shift+arrows and friends move the caret.
                reset(&format!("nav key vk={:#x}", vk.0), true);
                return false;
            }
            Err(()) => {
                reset("dead key", true);
                return false;
            }
        },
    };
    let what = match key {
        Key::Char(c) if c.is_alphanumeric() => "key letter".to_string(),
        Key::Char(c) if c.is_whitespace() => "key space".to_string(),
        Key::Char(_) => "key symbol".to_string(),
        k => format!("key {k:?}"),
    };
    let eaten = run(&what, |e, ctx| e.key(key, ctx));
    spell(Job::Dirty { hide: false });
    eaten
}

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 {
            let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            if k.dwExtraInfo != MAGIC && active() {
                let msg = wparam.0 as u32;
                if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                    CLICKS.fetch_add(1, Ordering::Relaxed); // typing cancels a pending hover menu
                    if on_key_down(k) {
                        EATEN.with_borrow_mut(|s| s.insert(k.vkCode));
                        return LRESULT(1);
                    }
                } else if EATEN.with_borrow_mut(|s| s.remove(&k.vkCode)) {
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code >= 0 && active() {
            let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
            let msg = wparam.0 as u32;
            if m.dwExtraInfo != MAGIC {
                match msg {
                    WM_LBUTTONDOWN => {
                        if let Some(i) = hit_popup(m.pt) {
                            run("click popup", |e, ctx| e.accept(i, ctx));
                            EAT_MOUSE_UP.set(true);
                            return LRESULT(1);
                        }
                        CLICKS.fetch_add(1, Ordering::Relaxed);
                        reset("left click", false);
                        spell(Job::Dirty { hide: false });
                    }
                    WM_LBUTTONUP if EAT_MOUSE_UP.get() => {
                        EAT_MOUSE_UP.set(false);
                        return LRESULT(1);
                    }
                    WM_MOUSEMOVE => hover(m.pt),
                    WM_RBUTTONDOWN => {
                        CLICKS.fetch_add(1, Ordering::Relaxed);
                        if let Some((scan_id, id)) = hit_squiggle(m.pt) {
                            // Right-click on an underlined word: show fixes.
                            let h = shared().popup_hwnd.load(Ordering::Relaxed);
                            let _ = PostMessageW(Some(HWND(h as _)), WM_APP_SPELLMENU, WPARAM(id), LPARAM(scan_id as isize));
                            EAT_RMOUSE_UP.set(true);
                            return LRESULT(1);
                        }
                        reset("other click", false);
                    }
                    WM_RBUTTONUP if EAT_RMOUSE_UP.get() => {
                        EAT_RMOUSE_UP.set(false);
                        return LRESULT(1);
                    }
                    WM_MOUSEWHEEL | WM_MOUSEHWHEEL => spell(Job::Dirty { hide: true }),
                    WM_MBUTTONDOWN | WM_XBUTTONDOWN => reset("other click", false),
                    _ => {}
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// Counts clicks and key presses, so a pending hover menu can tell the user did something meanwhile.
pub static CLICKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Mouse moved: tell the UI when it enters or leaves an underlined word (it opens the fixes after a short rest).
/// Staying inside the same word's box counts as the same hover, even when a rescan renumbers the words.
fn hover(p: POINT) {
    let inside = |r: &windows::Win32::Foundation::RECT| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom + 4;
    if HOVER_RECT.get().is_some_and(|r| inside(&r)) {
        return;
    }
    let hit = {
        let s = lock(&shared().squiggles);
        s.items.iter().find_map(|q| q.rects.iter().find(|r| inside(r)).copied())
    };
    let was = HOVER_RECT.replace(hit).is_some();
    if hit.is_some() || was {
        let h = shared().popup_hwnd.load(Ordering::Relaxed);
        let pt = ((p.x as u32 as u64) << 32 | p.y as u32 as u64) as isize;
        let _ = unsafe { PostMessageW(Some(HWND(h as _)), WM_APP_SPELLHOVER, WPARAM(hit.is_some() as usize), LPARAM(pt)) };
    }
}

pub fn hit_squiggle(p: POINT) -> Option<(u64, usize)> {
    let s = lock(&shared().squiggles);
    s.items
        .iter()
        .find(|q| q.rects.iter().any(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom + 4))
        .map(|q| (s.scan_id, q.id))
}

fn hit_popup(p: POINT) -> Option<usize> {
    let items = lock(&shared().popup_items);
    items.iter().position(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom)
}

//! Windows' built-in spell checker, used as a second opinion: if Windows knows a word
//! (tech words like "toggle" that our frequency lists miss), we don't correct or underline it.

use qalam::dict::Lang;
use std::cell::RefCell;
use std::collections::HashMap;
use windows::Win32::Globalization::{ISpellChecker, ISpellCheckerFactory, ISpellingError, SpellCheckerFactory};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::HSTRING;

thread_local! {
    /// Per thread (hooks / spell / UI each ask): None = that language isn't installed.
    static CHECKERS: RefCell<Option<HashMap<Lang, Option<ISpellChecker>>>> = const { RefCell::new(None) };
}

fn tag(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "en-US",
        Lang::Ar => "ar-SA",
    }
}

fn checker(lang: Lang) -> Option<ISpellChecker> {
    CHECKERS.with_borrow_mut(|m| {
        let m = m.get_or_insert_with(|| {
            // Fine if COM is already set up on this thread (S_FALSE / RPC_E_CHANGED_MODE).
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            HashMap::new()
        });
        m.entry(lang)
            .or_insert_with(|| unsafe {
                let f: ISpellCheckerFactory = CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).ok()?;
                let t = HSTRING::from(tag(lang));
                if !f.IsSupported(&t).ok()?.as_bool() {
                    return None;
                }
                f.CreateSpellChecker(&t).ok()
            })
            .clone()
    })
}

type Ask = (String, Lang, std::sync::mpsc::Sender<bool>);

/// The checker lives on its own thread: it can be slow (the first call loads a dictionary), and it's
/// asked from inside the keyboard hook, which Windows drops if it stalls.
fn worker() -> &'static std::sync::Mutex<std::sync::mpsc::Sender<Ask>> {
    static W: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<Ask>>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Ask>();
        std::thread::Builder::new()
            .name("winspell".into())
            .spawn(move || {
                let _ = checker(Lang::En); // warm up
                for (w, l, reply) in rx {
                    let _ = reply.send(check(&w, l));
                }
            })
            .ok();
        std::sync::Mutex::new(tx)
    })
}

/// Start the checker thread early so the dictionary is loaded before the first word.
pub fn warm_up() {
    let _ = worker();
}

fn check(word: &str, lang: Lang) -> bool {
    let Some(c) = checker(lang) else { return false };
    unsafe {
        let Ok(errs) = c.Check(&HSTRING::from(word)) else { return false };
        let mut e: Option<ISpellingError> = None;
        let hr = errs.Next(&mut e);
        hr.is_ok() && e.is_none()
    }
}

/// Does Windows consider `word` correctly spelled? False if unsure (no checker, or no answer within 40 ms).
pub fn knows(word: &str, lang: Lang) -> bool {
    static CACHE: std::sync::Mutex<Option<HashMap<(String, Lang), bool>>> = std::sync::Mutex::new(None);
    let key = (word.to_lowercase(), lang);
    if let Some(v) = CACHE.lock().ok().and_then(|c| c.as_ref().and_then(|m| m.get(&key).copied())) {
        return v;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    if worker().lock().map(|w| w.send((word.to_string(), lang, tx))).is_err() {
        return false;
    }
    match rx.recv_timeout(std::time::Duration::from_millis(40)) {
        Ok(v) => {
            if let Ok(mut c) = CACHE.lock() {
                let m = c.get_or_insert_with(HashMap::new);
                if m.len() > 20_000 {
                    m.clear();
                }
                m.insert(key, v);
            }
            v
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_knows_toggle() {
        if checker(Lang::En).is_none() {
            eprintln!("NO_CHECKER");
            return; // no English spell checker on this machine (CI images may lack it)
        }
        assert!(check("toggle", Lang::En));
        assert!(!check("toggel", Lang::En));
        warm_up();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(knows("toggle", Lang::En));
    }
}

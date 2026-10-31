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

/// Does Windows consider `word` correctly spelled? False if unsure (no checker for that language).
pub fn knows(word: &str, lang: Lang) -> bool {
    let Some(c) = checker(lang) else { return false };
    unsafe {
        let Ok(errs) = c.Check(&HSTRING::from(word)) else { return false };
        let mut e: Option<ISpellingError> = None;
        let hr = errs.Next(&mut e);
        hr.is_ok() && e.is_none()
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
        assert!(knows("toggle", Lang::En));
        assert!(knows("refactor", Lang::En) || knows("toggled", Lang::En));
        assert!(!knows("toggel", Lang::En));
    }
}

//! UI Automation work on its own thread (a hung app can't freeze typing or the popup):
//! finding misspelled words in the focused text box for squiggles, the corrected word's position for the
//! highlight, and replacing a word picked from the right-click menu.

use crate::{WM_APP_FLASH, WM_APP_SQUIGGLES, caret, lock, process_name_pid, shared};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};
use qalam::dict::is_mark;
use qalam::engine::Out;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, SAFEARRAY};
use windows::Win32::System::Ole::{SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{BOOL, BSTR};

pub enum Job {
    /// An autocorrect just typed a word of this many characters, followed by `sep` characters.
    Flash { word: usize, sep: usize },
    /// Text or layout may have changed: rescan soon. `hide` clears squiggles right away (scroll, move, focus).
    Dirty { hide: bool },
    /// The typing engine lost track of the text left of the caret (reset `epoch`): read it from the screen.
    Sync { epoch: u64 },
    /// Replace squiggle `id` from scan `scan_id` with `text`.
    Replace { scan_id: u64, id: usize, text: String },
}

#[derive(Clone)]
pub struct Squiggle {
    pub id: usize,
    pub word: String,
    pub rects: Vec<RECT>,
}

#[derive(Default)]
pub struct Squiggles {
    pub scan_id: u64,
    pub items: Vec<Squiggle>,
}

pub fn spawn() -> Sender<Job> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("spell".into()).spawn(move || run(rx)).expect("spawn spell");
    tx
}

fn post_ui(msg: u32) {
    let h = shared().popup_hwnd.load(Ordering::Relaxed);
    unsafe {
        let _ = PostMessageW(Some(HWND(h as _)), msg, WPARAM(0), LPARAM(0));
    }
}

fn set_squiggles(items: Vec<Squiggle>) {
    {
        let mut s = lock(&shared().squiggles);
        if s.items.is_empty() && items.is_empty() {
            return;
        }
        s.scan_id += 1;
        s.items = items;
    }
    post_ui(WM_APP_SQUIGGLES);
}

fn underline_on() -> bool {
    let s = shared();
    !s.paused.load(Ordering::Relaxed)
        && !s.disabled.load(Ordering::Relaxed)
        && !s.fullscreen.load(Ordering::Relaxed)
        && !s.replacements_only.load(Ordering::Relaxed)
        && lock(&s.engine).cfg.general.underline_misspelled
}

fn run(rx: Receiver<Job>) {
    while !crate::shared_ready() {
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Ok(auto) = (unsafe { CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }) else {
        crate::dlog(|| "spell: UI Automation unavailable".into());
        return;
    };
    let mut ranges: Vec<IUIAutomationTextRange> = Vec::new();
    let mut gen_of_ranges = 0u64;
    let mut due: Option<Instant> = None;
    let mut sync: Option<(u64, Instant)> = None;
    // Wake often enough for a quick sync.
    let mut last_scan = Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(if sync.is_some() { 20 } else { 100 })) {
            Ok(Job::Flash { word, sep }) => {
                // Give the app a moment to apply the typed correction.
                std::thread::sleep(Duration::from_millis(70));
                let r = flash_rect(&auto, word, sep);
                crate::dlog(|| format!("flash rect {r:?}"));
                if let Some(r) = r {
                    *lock(&shared().flash_rect) = Some(r);
                    post_ui(WM_APP_FLASH);
                }
                due = Some(Instant::now() + Duration::from_millis(150));
            }
            Ok(Job::Dirty { hide }) => {
                if hide {
                    ranges.clear();
                    set_squiggles(vec![]);
                }
                due = Some(Instant::now() + Duration::from_millis(if hide { 350 } else { 300 }));
            }
            Ok(Job::Replace { scan_id, id, text }) => {
                if scan_id == gen_of_ranges
                    && let Some(r) = ranges.get(id)
                    && unsafe { r.Select() }.is_ok()
                {
                    std::thread::sleep(Duration::from_millis(40));
                    let _ = lock(&shared().sender).send(vec![Out::Text(text)]);
                    lock(&shared().engine).reset(true);
                }
                due = Some(Instant::now() + Duration::from_millis(250));
            }
            Ok(Job::Sync { epoch }) => {
                // Let a click settle the caret / the app process keys first.
                sync = Some((epoch, Instant::now() + Duration::from_millis(90)));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if let Some((epoch, at)) = sync
            && Instant::now() >= at
        {
            sync = None;
            do_sync(&auto, epoch);
        }
        let periodic = last_scan.elapsed() > Duration::from_millis(1500);
        if due.is_some_and(|d| Instant::now() >= d) || periodic {
            due = None;
            last_scan = Instant::now();
            if !underline_on() {
                ranges.clear();
                set_squiggles(vec![]);
                continue;
            }
            let (items, new_ranges) = scan(&auto).unwrap_or_default();
            ranges = new_ranges;
            set_squiggles(items);
            gen_of_ranges = lock(&shared().squiggles).scan_id;
        }
    }
}

fn bounding(r: &IUIAutomationTextRange) -> Vec<RECT> {
    let mut out = Vec::new();
    unsafe {
        let Ok(psa) = r.GetBoundingRectangles() else { return out };
        if psa.is_null() {
            return out;
        }
        let psa: *mut SAFEARRAY = psa;
        if let (Ok(lb), Ok(ub)) = (SafeArrayGetLBound(psa, 1), SafeArrayGetUBound(psa, 1)) {
            let n = (ub - lb + 1).max(0) as usize;
            let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
            if n >= 4 && SafeArrayAccessData(psa, &mut data).is_ok() {
                let v = std::slice::from_raw_parts(data as *const f64, n);
                for c in v.chunks_exact(4) {
                    if c[2] > 0.0 && c[3] > 0.0 {
                        out.push(RECT {
                            left: c[0] as i32,
                            top: c[1] as i32,
                            right: (c[0] + c[2]) as i32,
                            bottom: (c[1] + c[3]) as i32,
                        });
                    }
                }
                let _ = SafeArrayUnaccessData(psa);
            }
        }
        let _ = SafeArrayDestroy(psa);
    }
    out
}

fn text_of(r: &IUIAutomationTextRange, max: i32) -> String {
    unsafe { r.GetText(max).map(|b| b.to_string()).unwrap_or_default() }
}

fn focused_text(auto: &IUIAutomation) -> Option<(IUIAutomationElement, IUIAutomationTextPattern)> {
    unsafe {
        let el = auto.GetFocusedElement().ok()?;
        if el.CurrentIsPassword().is_ok_and(|b| b.as_bool()) {
            return None;
        }
        let pid = el.CurrentProcessId().ok()? as u32;
        if pid == std::process::id() {
            return None;
        }
        let tp: IUIAutomationTextPattern = el.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
        Some((el, tp))
    }
}

/// A collapsed range at the caret.
unsafe fn caret_range(el: &IUIAutomationElement, tp: &IUIAutomationTextPattern) -> Option<IUIAutomationTextRange> {
    unsafe {
        match el.GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id) {
            Ok(tp2) => {
                let mut active = BOOL(0);
                tp2.GetCaretRange(&mut active).ok()
            }
            Err(_) => tp.GetSelection().ok()?.GetElement(0).ok(),
        }
    }
}

/// Read the current line left of the caret and hand it to the engine.
fn do_sync(auto: &IUIAutomation, epoch: u64) {
    let read = (|| unsafe {
        let (el, tp) = focused_text(auto)?;
        // Read-only (page text, sliders, players): not a text box, nothing to resume.
        if let Ok(v) = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            && v.CurrentIsReadOnly().is_ok_and(|b| b.as_bool())
        {
            return None;
        }
        // Text selected: typing will replace it, nothing useful to read.
        if let Ok(sel) = tp.GetSelection()
            && let Ok(first) = sel.GetElement(0)
            && !text_of(&first, 2).is_empty()
        {
            return None;
        }
        let caret = caret_range(&el, &tp)?;
        let before = caret.Clone().ok()?;
        before.MoveEndpointByRange(TextPatternRangeEndpoint_End, &caret, TextPatternRangeEndpoint_Start).ok()?;
        before.MoveEndpointByRange(TextPatternRangeEndpoint_Start, &caret, TextPatternRangeEndpoint_Start).ok()?;
        before.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -200).ok()?;
        let text = text_of(&before, 400);
        let after = caret.Clone().ok()?;
        after.MoveEndpointByRange(TextPatternRangeEndpoint_End, &caret, TextPatternRangeEndpoint_Start).ok()?;
        after.MoveEndpointByRange(TextPatternRangeEndpoint_Start, &caret, TextPatternRangeEndpoint_Start).ok()?;
        after.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, 1).ok()?;
        let mid_word = text_of(&after, 4).chars().next().is_some_and(word_char);
        Some((text, mid_word))
    })();
    let Some((text, mid_word)) = read else {
        crate::dlog(|| format!("sync epoch={epoch}: no text access"));
        return;
    };
    let (ok, changed) = {
        let mut eng = lock(&shared().engine);
        let before = eng.popup_version;
        let ok = eng.seed(&text, mid_word, epoch, qalam::engine::Ctx::default());
        (ok, eng.popup_version != before)
    };
    crate::dlog(|| format!("sync epoch={epoch}: read {} chars, mid_word={mid_word}, applied={ok}", text.chars().count()));
    if changed {
        post_ui(crate::WM_APP_POPUP);
    }
}

/// Where the just-corrected word is on screen.
fn flash_rect(auto: &IUIAutomation, word: usize, sep: usize) -> Option<RECT> {
    let via_uia = (|| unsafe {
        let (el, tp) = focused_text(auto)?;
        let caret = caret_range(&el, &tp)?;
        let r = caret.Clone().ok()?;
        r.MoveEndpointByRange(TextPatternRangeEndpoint_Start, &caret, TextPatternRangeEndpoint_End).ok()?;
        r.MoveEndpointByRange(TextPatternRangeEndpoint_End, &caret, TextPatternRangeEndpoint_End).ok()?;
        r.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -((word + sep) as i32)).ok()?;
        r.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, -(sep as i32)).ok()?;
        let rects = bounding(&r);
        rects.first().copied()
    })();
    if via_uia.is_some() {
        return via_uia;
    }
    // Estimate from the caret: average character ~0.5 × line height.
    let (c, src) = caret::caret_rect();
    if matches!(src, "mouse" | "last" | "browser") {
        return None;
    }
    let h = (c.bottom - c.top).max(8) as f32;
    let right = c.left - (h * 0.3 * sep as f32) as i32;
    let left = right - (h * 0.5 * word as f32) as i32;
    Some(RECT { left, top: c.top, right, bottom: c.bottom })
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || is_mark(c)
}

/// Words in `text` worth spell checking: (char offset, word). Skips URLs, @mentions, #tags, emails, paths, code-ish tokens.
fn tokens(text: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !word_char(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && word_char(chars[i]) {
            i += 1;
        }
        // Trim apostrophes at the edges ('quoted').
        let (mut s, mut e) = (start, i);
        while s < e && chars[s] == '\'' {
            s += 1;
        }
        while e > s && chars[e - 1] == '\'' {
            e -= 1;
        }
        if e - s < 2 {
            continue;
        }
        let prev = if start > 0 { chars[start - 1] } else { ' ' };
        let next = chars.get(i).copied().unwrap_or(' ');
        let after_next = chars.get(i + 1).copied().unwrap_or(' ');
        if "@#/\\:_.-$%&=+~<>`*".contains(prev)
            || "@/\\:_`*<>=".contains(next)
            || (next == '.' && after_next.is_alphanumeric())
            || (next == '-' && after_next.is_alphanumeric())
        {
            continue;
        }
        out.push((s, chars[s..e].iter().collect()));
    }
    out
}

fn scan(auto: &IUIAutomation) -> Option<(Vec<Squiggle>, Vec<IUIAutomationTextRange>)> {
    unsafe {
        let (el, tp) = focused_text(auto)?;
        // Read-only documents (web pages, etc.) aren't being typed in: skip them.
        if let Ok(v) = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            && v.CurrentIsReadOnly().is_ok_and(|b| b.as_bool())
        {
            return None;
        }
        let name = process_name_pid(el.CurrentProcessId().ok()? as u32).unwrap_or_default();
        {
            // Check the text box's own app too (not just the foreground window): terminals and
            // turned-off apps are never scanned, since reading a big console buffer can stall it.
            let eng = lock(&shared().engine);
            let g = &eng.cfg.general;
            let listed = |l: &[String]| l.iter().any(|a| a.eq_ignore_ascii_case(&name));
            let off = if g.app_mode.eq_ignore_ascii_case("whitelist") { !listed(&g.enabled_apps) } else { listed(&g.disabled_apps) };
            if off || listed(&g.underline_skip_apps) || listed(&g.replacements_only_apps) {
                return None;
            }
        }
        let vis = tp.GetVisibleRanges().ok()?;
        let n = vis.Length().ok()?.min(4);
        let (caret, caret_src) = caret::caret_rect();
        let caret_known = matches!(caret_src, "win32" | "msaa" | "uia");
        let mut items = Vec::new();
        let mut ranges = Vec::new();
        for i in 0..n {
            let Ok(r) = vis.GetElement(i) else { continue };
            let text = text_of(&r, 6_000);
            // Our own lists under the engine lock (fast); the OS spell checker after releasing it,
            // so the keyboard hook never waits on it.
            let candidates: Vec<(usize, String, qalam::dict::Lang)> = {
                let eng = lock(&shared().engine);
                tokens(&text).into_iter().filter_map(|(i, w)| eng.misspelled_local(&w).map(|l| (i, w, l))).take(200).collect()
            };
            let bad: Vec<(usize, String)> =
                candidates.into_iter().filter(|(_, w, l)| !crate::winspell::knows(w, *l)).map(|(i, w, _)| (i, w)).take(80).collect();
            for (start, word) in bad {
                let len = word.chars().count() as i32;
                let Ok(wr) = r.Clone() else { continue };
                let moved = wr.MoveEndpointByRange(TextPatternRangeEndpoint_End, &r, TextPatternRangeEndpoint_Start).is_ok()
                    && wr.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, start as i32 + len).is_ok()
                    && wr.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, start as i32).is_ok();
                let wr = if moved && text_of(&wr, len + 4).trim() == word {
                    wr
                } else {
                    // Offsets drifted (emoji, combined characters...): search near the expected spot.
                    let Ok(search) = r.Clone() else { continue };
                    let _ = search.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, start.saturating_sub(16) as i32);
                    match search.FindText(&BSTR::from(word.as_str()), false, false) {
                        Ok(f) => f,
                        Err(_) => continue,
                    }
                };
                let rects = bounding(&wr);
                if rects.is_empty() {
                    continue;
                }
                // Don't nag about the word being typed right now.
                let at_caret = caret_known
                    && rects.iter().any(|q| {
                        caret.left >= q.left - 3 && caret.left <= q.right + 3 && caret.bottom > q.top && caret.top < q.bottom
                    });
                if at_caret {
                    continue;
                }
                items.push(Squiggle { id: ranges.len(), word, rects });
                ranges.push(wr);
            }
        }
        Some((items, ranges))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokenizer_skips_links_and_mentions() {
        let t = super::tokens("teh @bob see http://exmple.com and becuse #chanel it's 'quoted' x");
        let words: Vec<_> = t.iter().map(|(_, w)| w.as_str()).collect();
        assert_eq!(words, vec!["teh", "see", "and", "becuse", "it's", "quoted"]);
    }
}

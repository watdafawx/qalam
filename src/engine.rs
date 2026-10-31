//! Pure typing logic: tracks what was typed, decides completions, corrections and replacements.
//! No Win32 here, so it can be unit tested.

use crate::arabizi::{self, Arabizi};
use crate::config::Config;
use crate::dict::{Dict, LEARN_SUGGEST_MIN, Lang, Learned, is_mark, match_case, norm_key};
use std::collections::HashMap;

const HISTORY_MAX: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Backspace,
    CtrlBackspace,
    Enter,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
}

/// Something to type into the focused app.
#[derive(Clone, Debug, PartialEq)]
pub enum Out {
    Backspace(usize),
    Text(String),
    /// A line break inside a replacement (typed per the `newline` setting).
    Newline,
    Enter,
    Tab,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    /// Let the key through untouched.
    Pass,
    /// Eat the key.
    Swallow,
    /// Eat the key and type this instead.
    Send(Vec<Out>),
}

#[derive(Clone, Copy, Default)]
pub struct Ctx {
    /// Focus is in a password box: no suggestions, corrections or learning (replacements still work).
    pub password: bool,
    /// Language of the active keyboard layout (used when language_mode = "keyboard").
    pub kb_lang: Option<Lang>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub label: String,
    /// Characters to erase before inserting.
    pub delete: usize,
    pub insert: String,
}

struct LastFix {
    original: String,
    fixed_len: usize,
    hist_len: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Sep {
    Char(char),
    Enter,
    Tab,
}

pub struct Engine {
    dicts: HashMap<Lang, Dict>,
    arabizi: Option<Arabizi>,
    pub learned: Learned,
    pub cfg: Config,
    replacements: HashMap<String, String>,
    user_typos: HashMap<String, String>,
    history: Vec<char>,
    /// We don't know what's left of the caret (after arrows, paste...). Cleared by the next space.
    dirty: bool,
    pub suggestions: Vec<Suggestion>,
    pub selected: usize,
    /// Arrow-key navigation inside the popup (entered with Up, like Windows).
    pub nav: bool,
    /// Bumped whenever the popup content changes.
    pub popup_version: u64,
    last_fix: Option<LastFix>,
    /// Ctrl+A was the last thing pressed.
    all_selected: bool,
    /// Set when an autocorrect fires: (corrected word length, separator length), for the highlight.
    pub flash: Option<(usize, usize)>,
    /// Bumped whenever we lose track of the text left of the caret; a screen read for that epoch can restore it.
    pub epoch: u64,
    /// Second opinion from the OS spell checker: true = the word is spelled right (don't fix or underline it).
    pub system_knows: Option<Box<dyn Fn(&str, Lang) -> bool + Send>>,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || is_mark(c)
}

/// Characters that end a word and fire replacements/autocorrect (like a phone keyboard).
fn is_trigger_sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | ')' | '"' | '،' | '؟' | '؛')
}

impl Engine {
    pub fn new(learned: Learned, cfg: Config, secrets: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut e = Engine {
            dicts: HashMap::new(),
            arabizi: None,
            learned,
            cfg: Config::default(),
            replacements: HashMap::new(),
            user_typos: HashMap::new(),
            history: Vec::new(),
            dirty: false,
            suggestions: Vec::new(),
            selected: 0,
            nav: false,
            popup_version: 0,
            last_fix: None,
            all_selected: false,
            flash: None,
            epoch: 0,
            system_knows: None,
        };
        e.set_config(cfg, secrets);
        e
    }

    pub fn set_config(&mut self, cfg: Config, secrets: impl IntoIterator<Item = (String, String)>) {
        self.replacements = cfg.replacements.clone().into_iter().chain(secrets).collect();
        self.user_typos = cfg.autocorrect.iter().map(|(k, v)| (k.to_lowercase(), v.clone())).collect();
        self.cfg = cfg;
        // Load dictionaries on first use (Arabizi needs the Arabic one).
        let mut need = self.enabled_langs();
        if self.cfg.general.arabizi {
            need.push(Lang::Ar);
        }
        for l in need {
            self.dicts.entry(l).or_insert_with(|| Dict::load(l));
        }
        if self.cfg.general.arabizi && self.arabizi.is_none() {
            self.arabizi = self.dicts.get(&Lang::Ar).map(Arabizi::new);
        }
        self.reset(false);
    }

    /// Languages switched on in settings (English if none are valid).
    pub fn enabled_langs(&self) -> Vec<Lang> {
        let mut v: Vec<Lang> = self.cfg.general.languages.iter().filter_map(|s| Lang::from_code(s)).collect();
        v.dedup();
        if v.is_empty() {
            v.push(Lang::En);
        }
        v
    }

    fn autocorrect_lang(&self, l: Lang) -> bool {
        self.cfg.general.autocorrect_languages.iter().any(|s| Lang::from_code(s) == Some(l))
    }

    /// The dictionary to use for `word`, from its script, the enabled languages and the language mode.
    fn dict_for(&self, word: &str, ctx: Ctx) -> Option<&Dict> {
        let l = Lang::of_word(word)?;
        if !self.enabled_langs().contains(&l) {
            return None;
        }
        if self.cfg.general.language_mode.eq_ignore_ascii_case("keyboard")
            && let Some(kb) = ctx.kb_lang
            && kb != l
        {
            return None;
        }
        self.dicts.get(&l)
    }

    /// For the spell checker: should `word` get a squiggle? Uses every enabled language.
    pub fn misspelled(&self, word: &str) -> bool {
        let n = word.chars().count();
        if n < 2 || word.chars().any(|c| c.is_ascii_digit()) {
            return false;
        }
        // Names and acronyms (capitalised) are left alone, like most chat spell checkers.
        if word.chars().next().is_some_and(char::is_uppercase) {
            return false;
        }
        let Some(d) = self.dict_for(word, Ctx::default()) else { return false };
        let k = norm_key(word);
        if self.user_typos.contains_key(&k) || d.typos.contains_key(&k) {
            return true;
        }
        !d.known(word) && self.learned.get(word) < LEARN_SUGGEST_MIN && !self.system_knows(word, d.lang)
    }

    fn system_knows(&self, word: &str, lang: Lang) -> bool {
        self.system_knows.as_ref().is_some_and(|f| f(word, lang))
    }

    /// Spelling fixes for a word (right-click menu), best first.
    pub fn fixes_for(&self, word: &str, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        let k = norm_key(word);
        if let Some(f) = self.user_typos.get(&k) {
            out.push(match_case(word, f));
        }
        if let Some(d) = self.dict_for(word, Ctx::default()) {
            if let Some(f) = d.typos.get(&k) {
                out.push(match_case(word, f));
            }
            out.extend(d.fixes(word, &self.learned).into_iter().map(|f| match_case(word, &f)));
        }
        out.dedup();
        out.truncate(n);
        out
    }

    /// State summary for the debug log: lengths and flags only, never the text itself.
    pub fn debug_state(&self) -> String {
        format!(
            "hist={} word={} dirty={} sugg={} nav={}",
            self.history.len(),
            self.word().chars().count(),
            self.dirty,
            self.suggestions.len(),
            self.nav
        )
    }

    pub fn popup_visible(&self) -> bool {
        !self.suggestions.is_empty()
    }

    /// Forget the current context (click, focus change, shortcut...).
    pub fn reset(&mut self, dirty: bool) {
        self.history.clear();
        self.dirty = dirty;
        self.epoch += 1;
        self.last_fix = None;
        self.all_selected = false;
        self.hide_popup();
    }

    /// A Ctrl+letter shortcut was pressed. Most leave us not knowing what's left of the caret,
    /// but Ctrl+A followed by delete/cut/typing leaves an empty box, so the next word is a fresh start.
    pub fn shortcut(&mut self, letter: Option<char>) {
        match letter {
            Some('a') => {
                self.reset(false);
                self.all_selected = true;
            }
            Some('x') if self.all_selected => self.reset(false),
            _ => self.reset(true),
        }
    }

    fn hide_popup(&mut self) {
        if !self.suggestions.is_empty() || self.nav {
            self.suggestions.clear();
            self.nav = false;
            self.selected = 0;
            self.popup_version += 1;
        }
    }

    /// Trailing run of word characters (leading apostrophes stripped, so 'quoted' works).
    fn word(&self) -> String {
        let start = self.history.iter().rposition(|&c| !is_word_char(c)).map_or(0, |i| i + 1);
        let w: String = self.history[start..].iter().collect();
        w.trim_start_matches('\'').to_string()
    }

    /// Trailing run of non-whitespace (for triggers like ";pw" or "@@addr").
    /// The word is glued to other characters (a link, path, email, @handle, snake_case, version...):
    /// leave it alone. Only opening brackets/quotes may sit in front of a normal word.
    fn word_is_embedded(&self) -> bool {
        let token = self.token();
        let word = self.word();
        let prefix_len = token.chars().count().saturating_sub(word.chars().count());
        let prefix: String = token.chars().take(prefix_len).collect();
        !prefix.chars().all(|c| "([{\"'“‘«¿¡".contains(c))
    }

    fn token(&self) -> String {
        let start = self.history.iter().rposition(|c| c.is_whitespace()).map_or(0, |i| i + 1);
        self.history[start..].iter().collect()
    }

    fn push(&mut self, c: char) {
        self.history.push(c);
        if self.history.len() > HISTORY_MAX {
            self.history.drain(..self.history.len() - HISTORY_MAX);
        }
    }

    fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' {
                self.history.clear();
            } else {
                self.push(c);
            }
        }
    }

    fn truncate_chars(&mut self, n: usize) {
        let len = self.history.len();
        self.history.truncate(len.saturating_sub(n));
    }

    pub fn key(&mut self, key: Key, ctx: Ctx) -> Action {
        let last_fix = self.last_fix.take();
        if std::mem::take(&mut self.all_selected) && matches!(key, Key::Backspace | Key::CtrlBackspace) {
            // Everything was selected and is now deleted: empty box, clean start.
            self.reset(false);
            return Action::Pass;
        }
        match key {
            Key::Char(c) if is_trigger_sep(c) => self.on_separator(Sep::Char(c), ctx),
            Key::Char(c) => {
                self.push(c);
                self.refresh(ctx);
                Action::Pass
            }
            Key::Backspace => {
                if let Some(f) = last_fix
                    && f.hist_len == self.history.len()
                {
                    // Backspace right after an autocorrect puts the original word back and remembers it.
                    self.truncate_chars(f.fixed_len + 1);
                    self.push_str(&f.original);
                    self.learned.accept(&f.original.to_lowercase());
                    self.hide_popup();
                    return Action::Send(vec![Out::Backspace(f.fixed_len + 1), Out::Text(f.original)]);
                }
                if self.history.pop().is_none() {
                    self.dirty = true;
                    self.epoch += 1;
                }
                self.refresh(ctx);
                Action::Pass
            }
            Key::CtrlBackspace => {
                if self.history.is_empty() {
                    self.dirty = true;
                    self.epoch += 1;
                } else {
                    while self.history.last().is_some_and(|c| c.is_whitespace()) {
                        self.history.pop();
                    }
                    while self.history.last().is_some_and(|&c| is_word_char(c)) {
                        self.history.pop();
                    }
                }
                self.refresh(ctx);
                Action::Pass
            }
            Key::Enter if self.nav && self.popup_visible() => self.accept(self.selected, ctx),
            Key::Enter => self.on_separator(Sep::Enter, ctx),
            Key::Tab if self.cfg.general.accept_with_tab && self.popup_visible() => self.accept(self.selected, ctx),
            Key::Tab => self.on_separator(Sep::Tab, ctx),
            Key::Escape if self.popup_visible() => {
                self.hide_popup();
                Action::Swallow
            }
            Key::Escape => Action::Pass,
            Key::Up if self.popup_visible() => {
                if !self.nav {
                    self.nav = true;
                    self.popup_version += 1;
                }
                Action::Swallow
            }
            Key::Down if self.nav => {
                self.nav = false;
                self.popup_version += 1;
                Action::Swallow
            }
            Key::Left | Key::Right if self.nav => {
                let n = self.suggestions.len();
                self.selected = if key == Key::Right { (self.selected + 1) % n } else { (self.selected + n - 1) % n };
                self.popup_version += 1;
                Action::Swallow
            }
            Key::Up | Key::Down | Key::Left | Key::Right => {
                self.reset(true);
                Action::Pass
            }
        }
    }

    /// Pick suggestion `i` (keyboard or mouse).
    pub fn accept(&mut self, i: usize, ctx: Ctx) -> Action {
        let Some(s) = self.suggestions.get(i).cloned() else { return Action::Pass };
        let mut insert = s.insert.clone();
        let mut outs = Vec::new();
        if s.delete > 0 {
            outs.push(Out::Backspace(s.delete));
        }
        self.truncate_chars(s.delete);
        self.push_str(&insert);
        let word = self.word();
        if !ctx.password {
            self.learn(&word);
        }
        if self.cfg.general.space_after_accept {
            insert.push(' ');
            self.push(' ');
        }
        outs.push(Out::Text(insert));
        self.hide_popup();
        Action::Send(outs)
    }

    fn sep_out(sep: Sep) -> Out {
        match sep {
            Sep::Char(c) => Out::Text(c.to_string()),
            Sep::Enter => Out::Enter,
            Sep::Tab => Out::Tab,
        }
    }

    fn push_sep(&mut self, sep: Sep) {
        match sep {
            Sep::Char(c) => self.push(c),
            Sep::Tab => self.push('\t'),
            Sep::Enter => self.history.clear(),
        }
        let ws = matches!(sep, Sep::Enter | Sep::Tab) || matches!(sep, Sep::Char(c) if c.is_whitespace());
        if ws {
            self.dirty = false;
        }
    }

    fn on_separator(&mut self, sep: Sep, ctx: Ctx) -> Action {
        self.hide_popup();
        if self.dirty {
            self.push_sep(sep);
            return Action::Pass;
        }

        // 1. Text replacement (also in password boxes: that's half the point).
        let token = self.token();
        let word = self.word();
        let hit = [token.as_str(), word.as_str()]
            .into_iter()
            .filter(|t| !t.is_empty())
            .find_map(|t| self.replacements.get(t).map(|e| (t.chars().count(), e.clone())));
        if let Some((del, expansion)) = hit {
            let mut outs = vec![Out::Backspace(del)];
            for (i, line) in expansion.split('\n').enumerate() {
                if i > 0 {
                    outs.push(Out::Newline);
                }
                if !line.is_empty() {
                    outs.push(Out::Text(line.to_string()));
                }
            }
            outs.push(Self::sep_out(sep));
            self.truncate_chars(del);
            self.push_str(&expansion);
            self.push_sep(sep);
            return Action::Send(outs);
        }

        if word.is_empty() || ctx.password {
            self.push_sep(sep);
            return Action::Pass;
        }
        if self.word_is_embedded() {
            // Part of a link/path/email: no autocorrect, and don't learn URL fragments as words.
            self.push_sep(sep);
            return Action::Pass;
        }

        // 2. Autocorrect.
        if self.cfg.general.autocorrect
            && let Some(fixed) = self.correction(&word, ctx)
        {
            let n = word.chars().count();
            self.flash = Some((fixed.chars().count(), 1));
            self.truncate_chars(n);
            self.push_str(&fixed);
            self.learn(&fixed);
            self.push_sep(sep);
            if matches!(sep, Sep::Char(_)) {
                self.last_fix = Some(LastFix {
                    original: word,
                    fixed_len: fixed.chars().count(),
                    hist_len: self.history.len(),
                });
            }
            return Action::Send(vec![Out::Backspace(n), Out::Text(fixed), Self::sep_out(sep)]);
        }

        self.learn(&word);
        self.push_sep(sep);
        Action::Pass
    }

    fn learn(&mut self, word: &str) {
        let w = norm_key(word);
        let plain = w.chars().all(|c| c.is_alphabetic() || c == '\'');
        let arabizi_word = self.cfg.general.arabizi && arabizi::looks_like_arabizi(&w);
        if w.chars().count() >= 2 && (plain || arabizi_word) {
            self.learned.bump(&w);
        }
    }

    /// The fix for a finished word, if it needs one.
    pub fn correction(&self, word: &str, ctx: Ctx) -> Option<String> {
        let lower = norm_key(word);
        if word.chars().any(|c| c.is_ascii_digit()) {
            return None;
        }
        // Mixed case like "iPhone" or "ALLCAPS": leave alone.
        if word.chars().skip(1).any(|c| c.is_uppercase()) {
            return None;
        }
        if let Some(fix) = self.user_typos.get(&lower) {
            return Some(match_case(word, fix));
        }
        if self.learned.get(&lower) > LEARN_SUGGEST_MIN {
            return None;
        }
        let dict = self.dict_for(word, ctx)?;
        if !self.autocorrect_lang(dict.lang) {
            return None;
        }
        if let Some(fix) = dict.typos.get(&lower) {
            let fixed = match_case(word, fix);
            return (fixed != word).then_some(fixed);
        }
        if word.chars().count() < 4 || dict.known(word) || self.learned.get(&lower) >= LEARN_SUGGEST_MIN {
            return None;
        }
        if self.system_knows(word, dict.lang) {
            return None;
        }
        let best = dict.fixes(word, &self.learned).into_iter().next()?;
        Some(match_case(word, &best))
    }

    /// Restore context from the text actually on screen left of the caret (read via UI Automation after a reset).
    /// Ignored if more resets happened since `epoch`, or if it doesn't match what was typed since.
    /// `mid_word`: a letter sits right after the caret, so we're editing inside a word: track it but don't autocorrect.
    pub fn seed(&mut self, before_caret: &str, mid_word: bool, epoch: u64, ctx: Ctx) -> bool {
        if epoch != self.epoch || self.all_selected {
            return false;
        }
        let line = before_caret.rsplit(['\n', '\r', '\u{2029}', '\u{000B}']).next().unwrap_or("");
        let typed: String = self.history.iter().collect();
        if !line.ends_with(&typed) {
            return false;
        }
        let chars: Vec<char> = line.chars().filter(|&c| c != '\u{FFFC}').collect();
        let start = chars.len().saturating_sub(HISTORY_MAX);
        self.history = chars[start..].to_vec();
        self.dirty = mid_word;
        self.last_fix = None;
        self.refresh(ctx);
        true
    }

    fn refresh(&mut self, ctx: Ctx) {
        let before = std::mem::take(&mut self.suggestions);
        self.nav = false;
        self.selected = 0;
        let g = &self.cfg.general;
        let word = self.word();
        let n = word.chars().count();
        let max = g.max_suggestions.max(1);
        let has_digit = word.chars().any(|c| c.is_ascii_digit());
        let arabizi_on = g.arabizi && self.arabizi.is_some() && arabizi::looks_like_arabizi(&word);
        let mut out: Vec<Suggestion> = Vec::new();
        if g.suggestions && !self.dirty && !ctx.password && n >= g.min_prefix.max(1) && !self.word_is_embedded() {
            if !has_digit && let Some(dict) = self.dict_for(&word, ctx) {
                // With Arabizi on, keep the last slot for the Arabic reading.
                let slots = if arabizi_on { max.saturating_sub(1).max(1) } else { max };
                for (_, w) in dict.complete(&word, slots, &self.learned) {
                    let display = match_case(&word, &w);
                    let head: String = display.chars().take(n).collect();
                    out.push(if head == word {
                        // Keep what was typed, add the rest: fewer keystrokes to replay.
                        Suggestion { label: display.clone(), delete: 0, insert: display.chars().skip(n).collect() }
                    } else {
                        Suggestion { label: display.clone(), delete: n, insert: display }
                    });
                }
                if out.is_empty() && n >= 3 && !dict.known(&word) {
                    for w in dict.fixes(&word, &self.learned).into_iter().take(slots) {
                        let display = match_case(&word, &w);
                        out.push(Suggestion { label: display.clone(), delete: n, insert: display });
                    }
                }
            }
            if arabizi_on && let Some(az) = &self.arabizi {
                let room = max.saturating_sub(out.len()).max(1);
                let room = if has_digit || out.is_empty() { max } else { room };
                if has_digit {
                    out.clear();
                }
                for ar in az.convert(&word, room) {
                    if out.len() >= max {
                        break;
                    }
                    out.push(Suggestion { label: ar.clone(), delete: n, insert: ar });
                }
            }
        }
        self.suggestions = out;
        if before != self.suggestions {
            self.popup_version += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn engine() -> Engine {
        let mut cfg: Config = toml::from_str(crate::config::DEFAULT_CONFIG).unwrap();
        cfg.replacements.insert(";pw".into(), "s3cret!".into());
        cfg.replacements.insert("@@addr".into(), "1 Example Street\nSometown\nAB1 2CD".into());
        Engine::new(Learned::default(), cfg, [])
    }

    fn engine_with(f: impl FnOnce(&mut Config)) -> Engine {
        let mut cfg: Config = toml::from_str(crate::config::DEFAULT_CONFIG).unwrap();
        f(&mut cfg);
        Engine::new(Learned::default(), cfg, [])
    }

    fn typ(e: &mut Engine, s: &str) -> Vec<Action> {
        s.chars().map(|c| e.key(Key::Char(c), Ctx::default())).collect()
    }

    fn last(e: &mut Engine, s: &str) -> Action {
        typ(e, s).pop().unwrap()
    }

    #[test]
    fn autocorrect_typo_list_and_case() {
        let mut e = engine();
        assert_eq!(
            last(&mut e, "Teh "),
            Action::Send(vec![Out::Backspace(3), Out::Text("The".into()), Out::Text(" ".into())])
        );
        assert_eq!(
            last(&mut e, "dont."),
            Action::Send(vec![Out::Backspace(4), Out::Text("don't".into()), Out::Text(".".into())])
        );
        assert_eq!(last(&mut e, " i "), Action::Send(vec![Out::Backspace(1), Out::Text("I".into()), Out::Text(" ".into())]));
    }

    #[test]
    fn autocorrect_edit_distance() {
        let mut e = engine();
        assert_eq!(
            last(&mut e, "becuse "),
            Action::Send(vec![Out::Backspace(6), Out::Text("because".into()), Out::Text(" ".into())])
        );
        // Real words and short words are left alone.
        assert_eq!(last(&mut e, "form "), Action::Pass);
        assert_eq!(last(&mut e, "brb "), Action::Pass);
    }

    #[test]
    fn backspace_undoes_autocorrect_and_learns() {
        let mut e = engine();
        typ(&mut e, "teh ");
        assert_eq!(
            e.key(Key::Backspace, Ctx::default()),
            Action::Send(vec![Out::Backspace(4), Out::Text("teh".into())])
        );
        assert_eq!(last(&mut e, " teh "), Action::Pass);
    }

    #[test]
    fn replacement_with_newlines() {
        let mut e = engine();
        typ(&mut e, "@@addr");
        let a = e.key(Key::Enter, Ctx::default());
        assert_eq!(
            a,
            Action::Send(vec![
                Out::Backspace(6),
                Out::Text("1 Example Street".into()),
                Out::Newline,
                Out::Text("Sometown".into()),
                Out::Newline,
                Out::Text("AB1 2CD".into()),
                Out::Enter,
            ])
        );
    }

    #[test]
    fn replacement_works_in_password_box() {
        let mut e = engine();
        let pw = Ctx { password: true, ..Default::default() };
        for c in ";pw".chars() {
            e.key(Key::Char(c), pw);
        }
        assert!(!e.popup_visible());
        assert_eq!(
            e.key(Key::Char(' '), pw),
            Action::Send(vec![Out::Backspace(3), Out::Text("s3cret!".into()), Out::Text(" ".into())])
        );
        assert!(e.learned.counts.is_empty());
    }

    #[test]
    fn completion_accept_with_tab() {
        let mut e = engine();
        typ(&mut e, "Becau");
        assert!(e.popup_visible());
        assert_eq!(e.suggestions[0].label, "Because");
        assert_eq!(e.key(Key::Tab, Ctx::default()), Action::Send(vec![Out::Text("se ".into())]));
        assert!(!e.popup_visible());
    }

    #[test]
    fn popup_navigation() {
        let mut e = engine();
        typ(&mut e, "th");
        assert_eq!(e.key(Key::Up, Ctx::default()), Action::Swallow);
        assert_eq!(e.key(Key::Right, Ctx::default()), Action::Swallow);
        assert_eq!(e.selected, 1);
        let want = e.suggestions[1].insert.clone() + " ";
        assert_eq!(e.key(Key::Enter, Ctx::default()), Action::Send(vec![Out::Text(want)]));
    }

    #[test]
    fn arrows_make_context_dirty_until_space() {
        let mut e = engine();
        e.key(Key::Left, Ctx::default());
        assert_eq!(last(&mut e, "teh "), Action::Pass); // could be the end of "uteh", don't touch
        assert!(matches!(last(&mut e, "teh "), Action::Send(_)));
    }

    #[test]
    fn select_all_then_delete_is_a_clean_start() {
        let mut e = engine();
        typ(&mut e, "hello there");
        e.shortcut(Some('a'));
        e.key(Key::Backspace, Ctx::default());
        typ(&mut e, "becau");
        assert!(e.popup_visible(), "first word after Ctrl+A, Backspace should get suggestions");

        // Ctrl+A then just typing replaces everything too.
        e.shortcut(Some('a'));
        typ(&mut e, "becau");
        assert!(e.popup_visible());

        // Ctrl+A, Ctrl+X empties the box.
        e.shortcut(Some('a'));
        e.shortcut(Some('x'));
        typ(&mut e, "becau");
        assert!(e.popup_visible());

        // Other shortcuts (paste) still mean "unknown text before the caret".
        e.shortcut(Some('v'));
        typ(&mut e, "becau");
        assert!(!e.popup_visible());
    }

    #[test]
    fn arabic_suggestions_and_no_autocorrect_by_default() {
        let mut e = engine_with(|c| c.general.languages = vec!["en".into(), "ar".into()]);
        typ(&mut e, "مرح");
        assert!(e.popup_visible());
        // Unknown-ish Arabic isn't auto-replaced unless "ar" is in autocorrect_languages.
        assert_eq!(last(&mut e, "ب انا "), Action::Pass);
        // English still corrected in "all" mode.
        assert!(matches!(last(&mut e, "teh "), Action::Send(_)));
    }

    #[test]
    fn arabic_off_means_no_arabic_popup() {
        let mut e = engine();
        typ(&mut e, "مرح");
        assert!(!e.popup_visible());
    }

    #[test]
    fn keyboard_mode_limits_to_layout_language() {
        let mut e = engine_with(|c| {
            c.general.languages = vec!["en".into(), "ar".into()];
            c.general.language_mode = "keyboard".into();
        });
        let ar_kb = Ctx { password: false, kb_lang: Some(Lang::Ar) };
        for ch in "becau".chars() {
            e.key(Key::Char(ch), ar_kb);
        }
        assert!(!e.popup_visible());
    }

    #[test]
    fn arabizi_suggests_arabic() {
        let mut e = engine_with(|c| c.general.arabizi = true);
        typ(&mut e, "mar7aba");
        assert!(e.suggestions.iter().any(|s| s.insert == "مرحبا"), "{:?}", e.suggestions);
        let i = e.suggestions.iter().position(|s| s.insert == "مرحبا").unwrap();
        assert_eq!(e.accept(i, Ctx::default()), Action::Send(vec![Out::Backspace(7), Out::Text("مرحبا ".into())]));
    }

    #[test]
    fn correction_sets_flash() {
        let mut e = engine();
        typ(&mut e, "teh ");
        assert_eq!(e.flash.take(), Some((3, 1)));
    }

    #[test]
    fn system_checker_vetoes_fixes() {
        let mut e = engine();
        typ(&mut e, "toggel ");
        e.flash = None;
        e.system_knows = Some(Box::new(|w, _| w == "toggle"));
        assert!(e.correction("toggle", Ctx::default()).is_none(), "OS knows it: leave it");
        assert!(!e.misspelled("toggle"));
    }

    #[test]
    fn misspelled_words() {
        let e = engine_with(|c| c.general.languages = vec!["en".into(), "ar".into()]);
        assert!(e.misspelled("becuse"));
        assert!(e.misspelled("teh"));
        assert!(!e.misspelled("because"));
        assert!(!e.misspelled("Spotifast"), "capitalised names are skipped");
        assert!(!e.misspelled("انا"), "loose Arabic spelling is fine");
        assert!(e.misspelled("ثقصفغ"));
        assert_eq!(e.fixes_for("becuse", 3)[0], "because");
    }

    #[test]
    fn seed_restores_half_typed_word() {
        let mut e = engine();
        typ(&mut e, "I said becau");
        e.reset(false); // clicked back into the box
        let ep = e.epoch;
        typ(&mut e, "s"); // typed before the screen read came back
        assert!(e.seed("I said becaus", false, ep, Ctx::default()));
        assert!(e.popup_visible());
        assert_eq!(e.suggestions[0].label, "because");
        // A stale read (another reset since) is ignored.
        e.reset(false);
        assert!(!e.seed("anything", false, ep, Ctx::default()));
    }

    #[test]
    fn seed_mid_word_disables_autocorrect() {
        let mut e = engine();
        e.reset(true);
        let ep = e.epoch;
        assert!(e.seed("teh", true, ep, Ctx::default()));
        assert_eq!(last(&mut e, " "), Action::Pass);
    }

    #[test]
    fn seed_uses_current_line_only() {
        let mut e = engine();
        e.reset(false);
        let ep = e.epoch;
        assert!(e.seed("first line\nte", false, ep, Ctx::default()));
        assert!(matches!(last(&mut e, "h "), Action::Send(_)));
    }

    #[test]
    fn links_paths_and_emails_are_left_alone() {
        let mut e = engine();
        assert_eq!(last(&mut e, "https://github.com/watdafawx/qalam "), Action::Pass);
        assert_eq!(last(&mut e, r"C:\Users\becuse "), Action::Pass);
        assert_eq!(last(&mut e, "me@exmple.com "), Action::Pass);
        assert_eq!(last(&mut e, "my_vairable "), Action::Pass);
        typ(&mut e, "github.com/becau");
        assert!(!e.popup_visible());
        // Brackets and quotes in front are fine.
        assert!(matches!(last(&mut e, " (teh "), Action::Send(_)));
        assert!(matches!(last(&mut e, " \"teh "), Action::Send(_)));
        assert!(e.learned.get("qalam") == 0, "URL pieces aren't learned");
    }

    #[test]
    fn backspace_tracks_history() {
        let mut e = engine();
        typ(&mut e, "tehx");
        e.key(Key::Backspace, Ctx::default());
        assert!(matches!(last(&mut e, " "), Action::Send(_)));
    }
}

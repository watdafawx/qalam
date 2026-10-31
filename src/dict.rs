//! Word dictionaries (one per language): prefix completion, "is this a word?" and single-edit corrections.

use std::collections::HashMap;

const EN_WORDS: &str = include_str!("../data/en_50k.txt");
const EN_CONTRACTIONS: &str = include_str!("../data/contractions.txt");
const EN_TYPOS: &str = include_str!("../data/typos.txt");
const AR_WORDS: &str = include_str!("../data/ar_50k.txt");

/// How much one personal use of a word is worth, in corpus-frequency units.
const LEARN_BOOST: u64 = 40_000;
/// Learned words not in the dictionary show up as suggestions (and stop being underlined) after this many uses.
pub const LEARN_SUGGEST_MIN: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Lang {
    En,
    Ar,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ar];

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ar => "ar",
        }
    }
    pub fn from_code(s: &str) -> Option<Lang> {
        match s.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Lang::En),
            "ar" | "arabic" => Some(Lang::Ar),
            _ => None,
        }
    }
    /// Language for a Windows primary language id (low 10 bits of a LANGID).
    pub fn from_langid(primary: u16) -> Option<Lang> {
        match primary {
            0x09 => Some(Lang::En),
            0x01 => Some(Lang::Ar),
            _ => None,
        }
    }
    /// Which language's script a letter belongs to.
    pub fn of_char(c: char) -> Option<Lang> {
        if is_arabic(c) {
            Some(Lang::Ar)
        } else if c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c) {
            Some(Lang::En)
        } else {
            None
        }
    }
    pub fn of_word(w: &str) -> Option<Lang> {
        w.chars().find_map(Lang::of_char)
    }
}

pub fn is_arabic(c: char) -> bool {
    matches!(c, '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' | '\u{08A0}'..='\u{08FF}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}')
}

/// Combining marks that live inside words: Arabic harakat/shadda/etc. and Latin combining accents.
pub fn is_mark(c: char) -> bool {
    matches!(c, '\u{0300}'..='\u{036F}' | '\u{0610}'..='\u{061A}' | '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{06D6}'..='\u{06DC}' | '\u{06DF}'..='\u{06E4}' | '\u{06E7}' | '\u{06E8}' | '\u{06EA}'..='\u{06ED}')
}

/// Lookup key: lowercase, without diacritics or tatweel.
pub fn norm_key(w: &str) -> String {
    w.chars().filter(|&c| !is_mark(c) && c != '\u{0640}').flat_map(char::to_lowercase).collect()
}

/// Looser Arabic key: hamza/alef forms, alef maqsura and ta marbuta folded together.
fn loose_key(w: &str) -> String {
    norm_key(w)
        .chars()
        .map(|c| match c {
            'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
            'ى' => 'ي',
            'ة' => 'ه',
            'ؤ' => 'و',
            'ئ' => 'ي',
            c => c,
        })
        .collect()
}

const AR_PREFIXES: [&str; 12] = ["وال", "بال", "كال", "فال", "لل", "ال", "و", "ف", "ب", "ل", "ك", "س"];
const AR_SUFFIXES: [&str; 20] =
    ["كما", "هما", "تما", "كم", "كن", "هم", "هن", "نا", "ها", "ني", "وا", "ون", "ين", "ان", "ات", "ة", "ه", "ك", "ي", "ت"];

#[derive(Clone, Debug)]
pub struct Entry {
    pub key: String,     // norm_key
    pub display: String, // how it's written ("I'm", "don't", "أنا")
    pub freq: u64,
}

pub struct Dict {
    pub lang: Lang,
    /// Sorted by key, for prefix ranges.
    pub entries: Vec<Entry>,
    index: HashMap<String, usize>,
    /// Arabic only: loose key -> most frequent entry.
    loose: HashMap<String, usize>,
    /// Built-in typo -> fix (English only).
    pub typos: HashMap<String, String>,
    alphabet: Vec<char>,
}

/// Personal word counts, persisted to learned.txt. Keys are `norm_key`s.
#[derive(Default, Clone)]
pub struct Learned {
    pub counts: HashMap<String, u32>,
    pub dirty: bool,
}

impl Learned {
    pub fn get(&self, w: &str) -> u32 {
        self.counts.get(&norm_key(w)).copied().unwrap_or(0)
    }
    pub fn bump(&mut self, w: &str) {
        *self.counts.entry(norm_key(w)).or_insert(0) += 1;
        self.dirty = true;
    }
    /// Mark a word as definitely wanted (undid an autocorrect, "Add to dictionary").
    pub fn accept(&mut self, w: &str) {
        let c = self.counts.entry(norm_key(w)).or_insert(0);
        *c = (*c).max(LEARN_SUGGEST_MIN + 1);
        self.dirty = true;
    }
    pub fn parse(text: &str) -> Self {
        let mut counts = HashMap::new();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if let (Some(w), Some(c)) = (it.next(), it.next())
                && let Ok(c) = c.parse()
            {
                counts.insert(w.to_string(), c);
            }
        }
        Learned { counts, dirty: false }
    }
    pub fn serialize(&self) -> String {
        let mut v: Vec<_> = self.counts.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        v.iter().map(|(w, c)| format!("{w} {c}\n")).collect()
    }
}

fn parse_list(text: &str, mut add: impl FnMut(&str, u64)) {
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        if let (Some(w), Some(f)) = (it.next(), it.next()) {
            add(w, f.parse().unwrap_or(0));
        }
    }
}

impl Dict {
    pub fn english() -> Self {
        let mut typos = HashMap::new();
        for line in EN_TYPOS.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((wrong, right)) = line.split_once(' ') {
                typos.insert(wrong.to_lowercase(), right.trim().to_string());
            }
        }
        let mut map: HashMap<String, Entry> = HashMap::new();
        let mut add = |w: &str, f: u64| {
            if !w.chars().all(|c| c.is_alphabetic() || c == '\'') {
                return;
            }
            let key = norm_key(w);
            // Single letters other than a/I are noise in a subtitle corpus.
            if key.chars().count() == 1 && key != "a" && key != "i" {
                return;
            }
            insert(&mut map, key, w, f);
        };
        parse_list(EN_WORDS, &mut add);
        parse_list(EN_CONTRACTIONS, &mut add);
        // Known typos must not count as real words (the corpus has "dont", "im", "alot").
        for wrong in typos.keys() {
            if wrong != "i" {
                map.remove(wrong);
            }
        }
        Self::build(Lang::En, map.into_values().collect(), typos)
    }

    pub fn arabic() -> Self {
        let mut map: HashMap<String, Entry> = HashMap::new();
        parse_list(AR_WORDS, |w, f| {
            if !w.chars().all(|c| is_arabic(c) && (c.is_alphabetic() || is_mark(c))) {
                return;
            }
            let key = norm_key(w);
            if key.chars().count() >= 1 {
                let display: String = w.chars().filter(|&c| !is_mark(c)).collect();
                insert(&mut map, key, &display, f);
            }
        });
        Self::build(Lang::Ar, map.into_values().collect(), HashMap::new())
    }

    pub fn load(lang: Lang) -> Self {
        match lang {
            Lang::En => Self::english(),
            Lang::Ar => Self::arabic(),
        }
    }

    fn build(lang: Lang, mut entries: Vec<Entry>, typos: HashMap<String, String>) -> Self {
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        let index = entries.iter().enumerate().map(|(i, e)| (e.key.clone(), i)).collect();
        let mut loose: HashMap<String, usize> = HashMap::new();
        if lang == Lang::Ar {
            for (i, e) in entries.iter().enumerate() {
                let k = loose_key(&e.key);
                match loose.get(&k) {
                    Some(&j) if entries[j].freq >= e.freq => {}
                    _ => {
                        loose.insert(k, i);
                    }
                }
            }
        }
        let mut alphabet: Vec<char> = entries.iter().flat_map(|e| e.key.chars()).collect();
        alphabet.sort_unstable();
        alphabet.dedup();
        Dict { lang, entries, index, loose, typos, alphabet }
    }

    #[cfg(test)]
    pub fn from_list(lang: Lang, words: &[(&str, u64)]) -> Self {
        let entries =
            words.iter().map(|(w, f)| Entry { key: norm_key(w), display: w.to_string(), freq: *f }).collect();
        Self::build(lang, entries, HashMap::new())
    }

    /// Exact dictionary word (ignoring case and diacritics).
    pub fn contains(&self, w: &str) -> bool {
        self.index.contains_key(&norm_key(w))
    }

    /// Plausibly a real word: exact, or for Arabic a loose spelling or a known stem with common prefixes/suffixes.
    pub fn known(&self, w: &str) -> bool {
        if self.contains(w) {
            return true;
        }
        if self.lang != Lang::Ar {
            return false;
        }
        let k = norm_key(w);
        if self.loose.contains_key(&loose_key(&k)) {
            return true;
        }
        let stem_ok = |s: &str| s.chars().count() >= 2 && (self.index.contains_key(s) || self.loose.contains_key(&loose_key(s)));
        for p in std::iter::once("").chain(AR_PREFIXES) {
            let Some(rest) = k.strip_prefix(p) else { continue };
            if !p.is_empty() && stem_ok(rest) {
                return true;
            }
            for s in AR_SUFFIXES {
                if let Some(stem) = rest.strip_suffix(s)
                    && stem_ok(stem)
                {
                    return true;
                }
            }
        }
        false
    }

    pub fn freq(&self, w: &str) -> Option<u64> {
        self.index.get(&norm_key(w)).map(|&i| self.entries[i].freq)
    }

    fn display_of(&self, key: &str) -> String {
        self.index.get(key).map(|&i| self.entries[i].display.clone()).unwrap_or_else(|| key.to_string())
    }

    /// Up to `n` completions of `prefix` (longer than it), best first, as (score, word in dictionary spelling).
    pub fn complete(&self, prefix: &str, n: usize, learned: &Learned) -> Vec<(u64, String)> {
        let p = norm_key(prefix);
        let start = self.entries.partition_point(|e| e.key.as_str() < p.as_str());
        let mut scored: Vec<(u64, String)> = Vec::new();
        for e in &self.entries[start..] {
            if !e.key.starts_with(&p) {
                break;
            }
            if e.key.len() > p.len() {
                scored.push((e.freq + learned.get(&e.key) as u64 * LEARN_BOOST, e.display.clone()));
            }
        }
        for (w, &c) in &learned.counts {
            if c >= LEARN_SUGGEST_MIN
                && w.len() > p.len()
                && w.starts_with(&p)
                && !self.index.contains_key(w)
                && Lang::of_word(w).is_none_or(|l| l == self.lang)
            {
                scored.push((c as u64 * LEARN_BOOST, w.clone()));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(n);
        scored
    }

    /// Spelling fixes for `word`, best first: for Arabic the loose (hamza/ta marbuta) match first,
    /// then dictionary words one edit away.
    pub fn fixes(&self, word: &str, learned: &Learned) -> Vec<String> {
        let w = norm_key(word);
        let mut out: Vec<(u64, String)> = Vec::new();
        if self.lang == Lang::Ar
            && let Some(&i) = self.loose.get(&loose_key(&w))
            && self.entries[i].key != w
        {
            out.push((u64::MAX, self.entries[i].display.clone()));
        }
        let chars: Vec<char> = w.chars().collect();
        let mut seen: HashMap<String, u64> = HashMap::new();
        let mut consider = |s: String| {
            if let Some(&i) = self.index.get(&s) {
                seen.insert(s, self.entries[i].freq + learned.get(&self.entries[i].key) as u64 * LEARN_BOOST);
            }
        };
        let mut letters = self.alphabet.clone();
        if self.lang == Lang::En && !letters.contains(&'\'') {
            letters.push('\'');
        }
        for i in 0..=chars.len() {
            let l: String = chars[..i].iter().collect();
            let r = &chars[i..];
            if !r.is_empty() {
                let tail: String = r[1..].iter().collect();
                consider(format!("{l}{tail}"));
                if r.len() > 1 {
                    consider(format!("{l}{}{}{}", r[1], r[0], r[2..].iter().collect::<String>()));
                }
                for &c in &letters {
                    if c != r[0] {
                        consider(format!("{l}{c}{tail}"));
                    }
                }
            }
            let rs: String = r.iter().collect();
            for &c in &letters {
                consider(format!("{l}{c}{rs}"));
            }
        }
        out.extend(seen.into_iter().map(|(k, f)| (f, self.display_of(&k))));
        out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        out.dedup_by(|a, b| a.1 == b.1);
        out.into_iter().map(|(_, w)| w).collect()
    }
}

fn insert(map: &mut HashMap<String, Entry>, key: String, display: &str, freq: u64) {
    let e = map.entry(key.clone()).or_insert(Entry { key, display: display.to_string(), freq: 0 });
    if freq >= e.freq {
        e.display = display.to_string();
    }
    e.freq = e.freq.max(freq);
}

/// Re-case `word` (dictionary spelling) to match how the user typed `typed`. No-op for scripts without case.
pub fn match_case(typed: &str, word: &str) -> String {
    let letters: Vec<char> = typed.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase()) {
        return word.to_uppercase();
    }
    if letters.first().is_some_and(|c| c.is_uppercase()) {
        let mut cs = word.chars();
        return match cs.next() {
            Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
            None => String::new(),
        };
    }
    word.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_loads_and_completes() {
        let d = Dict::english();
        let l = Learned::default();
        assert_eq!(d.complete("th", 3, &l)[0].1, "the");
        assert!(d.contains("don't"));
        assert!(!d.contains("dont"));
        assert_eq!(d.complete("i'", 1, &l)[0].1, "I'm");
    }

    #[test]
    fn english_fixes() {
        let d = Dict::english();
        let l = Learned::default();
        assert_eq!(d.fixes("becuse", &l)[0], "because");
        assert_eq!(d.fixes("hwat", &l)[0], "what");
    }

    #[test]
    fn arabic_loads_completes_and_knows_forms() {
        let d = Dict::arabic();
        let l = Learned::default();
        assert!(d.contains("أنا"));
        assert!(d.contains("أَنا"), "diacritics ignored");
        assert!(!d.complete("مرح", 3, &l).is_empty());
        // Loose spelling without hamza is "known" and fixed to the dictionary form.
        assert!(d.known("انا"));
        assert_eq!(d.fixes("انا", &l)[0], "أنا");
        // Prefix + suffix on a known stem.
        assert!(d.known("وكتابها") || d.known("والكتاب"));
        assert!(!d.known("ثقصفغ"));
    }

    #[test]
    fn learned_words_suggest() {
        let d = Dict::from_list(Lang::En, &[("spot", 100)]);
        let mut l = Learned::default();
        l.bump("spotifast");
        assert_eq!(d.complete("spo", 3, &l).len(), 1);
        l.bump("spotifast");
        assert!(d.complete("spo", 3, &l).iter().any(|(_, w)| w == "spotifast"));
    }

    #[test]
    fn scripts() {
        assert_eq!(Lang::of_word("hello"), Some(Lang::En));
        assert_eq!(Lang::of_word("مرحبا"), Some(Lang::Ar));
        assert_eq!(Lang::of_word("123"), None);
    }

    #[test]
    fn casing() {
        assert_eq!(match_case("Th", "the"), "The");
        assert_eq!(match_case("TH", "the"), "THE");
        assert_eq!(match_case("th", "I'm"), "I'm");
        assert_eq!(match_case("مر", "مرحبا"), "مرحبا");
    }
}

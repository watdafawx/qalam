//! Arabizi -> Arabic: "mar7aba" -> "مرحبا". Each Latin letter/digit maps to a few possible Arabic letters
//! (vowels may be dropped); a beam search walks the Arabic dictionary as a trie and ranks by word frequency.

use crate::dict::Dict;
use std::collections::HashMap;

struct Node {
    children: HashMap<char, u32>,
    /// (frequency, entry index) if a word ends here.
    word: Option<(u64, u32)>,
    /// Best word anywhere below (for completions).
    best: (u64, u32),
}

pub struct Arabizi {
    nodes: Vec<Node>,
    words: Vec<String>,
}

type Opts = &'static [(&'static str, f32)];

/// Possible readings of the Latin text at `pos`: (chars consumed, [(arabic, penalty)]).
fn rules(s: &[char], pos: usize) -> Vec<(usize, Opts)> {
    let c = s[pos];
    let next = s.get(pos + 1).copied();
    let start = pos == 0;
    let last = pos + 1 == s.len();
    let mut out: Vec<(usize, Opts)> = Vec::new();

    // Two-character units first.
    if let Some(n) = next {
        let two_last = pos + 2 == s.len();
        let o: Option<Opts> = match (c, n) {
            ('s', 'h') => Some(&[("ش", 0.0)]),
            ('c', 'h') => Some(&[("ش", 0.3)]),
            ('k', 'h') => Some(&[("خ", 0.0)]),
            ('g', 'h') => Some(&[("غ", 0.0)]),
            ('t', 'h') => Some(&[("ث", 0.0), ("ذ", 0.5), ("ت", 1.0)]),
            ('d', 'h') => Some(&[("ذ", 0.0), ("ض", 0.3)]),
            ('o', 'u') | ('o', 'o') | ('u', 'u') => Some(&[("و", 0.0)]),
            ('e', 'e') | ('i', 'i') => Some(&[("ي", 0.0)]),
            ('a', 'a') => Some(&[("ا", 0.0), ("ع", 0.6)]),
            ('3', '\'') => Some(&[("غ", 0.0)]),
            ('7', '\'') => Some(&[("خ", 0.0)]),
            ('6', '\'') => Some(&[("ظ", 0.0)]),
            ('9', '\'') => Some(&[("ض", 0.0)]),
            ('a', 'h') | ('e', 'h') if two_last => Some(&[("ة", 0.0), ("ه", 0.5)]),
            ('a', 'n') if two_last => Some(&[("ا", 0.2), ("ان", 0.0)]),
            ('e', 'n') | ('i', 'n') if two_last => Some(&[("ين", 0.0), ("", 0.8)]),
            _ => None,
        };
        if let Some(o) = o {
            out.push((2, o));
        }
    }

    let o: Opts = match c {
        '2' => &[("أ", 0.0), ("ء", 0.2), ("إ", 0.4), ("ق", 0.6), ("ئ", 0.6), ("ؤ", 0.6), ("آ", 0.8)],
        '3' => &[("ع", 0.0)],
        '5' => &[("خ", 0.0)],
        '6' => &[("ط", 0.0)],
        '7' => &[("ح", 0.0)],
        '8' => &[("غ", 0.3), ("ق", 0.3)],
        '9' => &[("ص", 0.0), ("ق", 0.3)],
        'a' if start => &[("ا", 0.0), ("أ", 0.0), ("ع", 0.6), ("آ", 1.0)],
        'a' if last => &[("ا", 0.0), ("ة", 0.2), ("ى", 0.4), ("", 0.6)],
        'a' => &[("", 0.0), ("ا", 0.3), ("ع", 1.2)],
        'e' if start => &[("إ", 0.0), ("ا", 0.2), ("", 1.0)],
        'e' if last => &[("ة", 0.3), ("", 0.4), ("ه", 0.5), ("ي", 0.6)],
        'e' => &[("", 0.0), ("ي", 0.6), ("ا", 1.0)],
        'i' if start => &[("إ", 0.0), ("ا", 0.3), ("ي", 0.6)],
        'i' if last => &[("ي", 0.0), ("ى", 0.5)],
        'i' => &[("ي", 0.0), ("", 0.4)],
        'o' if start => &[("أ", 0.0), ("ا", 0.4), ("و", 0.5)],
        'o' => &[("و", 0.0), ("", 0.5)],
        'u' if start => &[("أ", 0.0), ("ا", 0.4)],
        'u' => &[("و", 0.0), ("", 0.4)],
        'y' => &[("ي", 0.0), ("ى", 0.4)],
        'w' => &[("و", 0.0)],
        'b' | 'p' => &[("ب", 0.0)],
        'c' => &[("ك", 0.0), ("س", 0.5)],
        'd' => &[("د", 0.0), ("ض", 0.6)],
        'f' | 'v' => &[("ف", 0.0)],
        'g' => &[("ج", 0.0), ("غ", 0.5), ("ق", 0.7)],
        'h' if last => &[("ه", 0.0), ("ح", 0.4), ("ة", 0.5)],
        'h' => &[("ه", 0.0), ("ح", 0.4)],
        'j' => &[("ج", 0.0)],
        'k' => &[("ك", 0.0), ("ق", 0.5)],
        'l' => &[("ل", 0.0)],
        'm' => &[("م", 0.0)],
        'n' => &[("ن", 0.0)],
        'q' => &[("ق", 0.0)],
        'r' => &[("ر", 0.0)],
        's' => &[("س", 0.0), ("ص", 0.5)],
        't' if last => &[("ت", 0.0), ("ة", 0.4), ("ط", 0.5)],
        't' => &[("ت", 0.0), ("ط", 0.5)],
        'x' => &[("كس", 0.0)],
        'z' => &[("ز", 0.0), ("ظ", 0.6), ("ذ", 0.8)],
        '\'' => &[("ء", 0.3), ("ع", 0.4), ("", 0.6)],
        _ => &[],
    };
    if !o.is_empty() {
        out.push((1, o));
        // Doubled consonant = one letter with shadda ("habbibi", "yalla").
        if next == Some(c) && !"aeiou".contains(c) && c.is_ascii_alphabetic() {
            out.push((2, o));
        }
    }
    out
}

/// Worth trying as Arabizi: plain ASCII letters/digits/apostrophes with at least one letter.
pub fn looks_like_arabizi(w: &str) -> bool {
    w.chars().count() >= 2
        && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '\'')
        && w.chars().any(|c| c.is_ascii_alphabetic())
}

pub fn has_arabizi_digit(w: &str) -> bool {
    w.chars().any(|c| matches!(c, '2' | '3' | '5' | '6' | '7' | '8' | '9'))
}

impl Arabizi {
    pub fn new(dict: &Dict) -> Self {
        let mut nodes = vec![Node { children: HashMap::new(), word: None, best: (0, 0) }];
        let mut words = Vec::with_capacity(dict.entries.len());
        for (i, e) in dict.entries.iter().enumerate() {
            words.push(e.display.clone());
            let mut n = 0usize;
            for ch in e.key.chars() {
                n = match nodes[n].children.get(&ch) {
                    Some(&c) => c as usize,
                    None => {
                        nodes.push(Node { children: HashMap::new(), word: None, best: (0, 0) });
                        let id = nodes.len() - 1;
                        nodes[n].children.insert(ch, id as u32);
                        id
                    }
                };
            }
            if nodes[n].word.is_none_or(|(f, _)| e.freq > f) {
                nodes[n].word = Some((e.freq, i as u32));
            }
        }
        // Children are always created after parents, so a reverse pass fills `best` bottom-up.
        for n in (0..nodes.len()).rev() {
            let mut best = nodes[n].word.unwrap_or((0, 0));
            for &c in nodes[n].children.values() {
                if nodes[c as usize].best.0 > best.0 {
                    best = nodes[c as usize].best;
                }
            }
            nodes[n].best = best;
        }
        Arabizi { nodes, words }
    }

    fn walk(&self, mut n: usize, s: &str) -> Option<usize> {
        for ch in s.chars() {
            n = *self.nodes[n].children.get(&ch)? as usize;
        }
        Some(n)
    }

    /// Best Arabic readings of `latin`, best first. Includes completions of a partly typed word.
    pub fn convert(&self, latin: &str, n: usize) -> Vec<String> {
        const BEAM: usize = 400;
        let s: Vec<char> = latin.to_ascii_lowercase().chars().collect();
        if s.is_empty() {
            return vec![];
        }
        // states[pos]: trie node -> lowest penalty so far.
        let mut states: Vec<HashMap<usize, f32>> = vec![HashMap::new(); s.len() + 1];
        states[0].insert(0, 0.0);
        for pos in 0..s.len() {
            let mut cur: Vec<(usize, f32)> = states[pos].drain().collect();
            if cur.len() > BEAM {
                let score = |&(node, pen): &(usize, f32)| self.nodes[node].best.0 as f64 * (-1.2 * pen as f64).exp();
                cur.sort_by(|a, b| score(b).total_cmp(&score(a)));
                cur.truncate(BEAM);
            }
            for (len, opts) in rules(&s, pos) {
                for &(node, pen) in &cur {
                    for &(ar, p) in opts {
                        if let Some(next) = self.walk(node, ar) {
                            let slot = states[pos + len].entry(next).or_insert(f32::MAX);
                            *slot = slot.min(pen + p);
                        }
                    }
                }
            }
        }
        let mut found: HashMap<u32, f64> = HashMap::new();
        for (&node, &pen) in &states[s.len()] {
            if node == 0 {
                continue;
            }
            let nd = &self.nodes[node];
            if let Some((f, i)) = nd.word {
                let sc = f as f64 * (-1.2 * pen as f64).exp();
                let e = found.entry(i).or_insert(0.0);
                *e = e.max(sc);
            }
            let (f, i) = nd.best;
            if f > 0 {
                let sc = f as f64 * (-1.2 * (pen + 0.8) as f64).exp();
                let e = found.entry(i).or_insert(0.0);
                *e = e.max(sc);
            }
        }
        let mut v: Vec<(f64, u32)> = found.into_iter().map(|(i, s)| (s, i)).collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        v.into_iter().take(n).map(|(_, i)| self.words[i as usize].clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_words() {
        let a = Arabizi::new(&Dict::arabic());
        let top = |w: &str| a.convert(w, 3);
        assert!(top("mar7aba").contains(&"مرحبا".to_string()), "{:?}", top("mar7aba"));
        assert!(top("ana").contains(&"أنا".to_string()), "{:?}", top("ana"));
        assert!(top("7abibi").iter().any(|w| w.starts_with("حبيب")), "{:?}", top("7abibi"));
        assert!(top("shukran").iter().any(|w| w.starts_with("شكر")), "{:?}", top("shukran"));
        assert!(top("3arabi").iter().any(|w| w.starts_with("عرب")), "{:?}", top("3arabi"));
        assert!(top("ya3ni").contains(&"يعني".to_string()), "{:?}", top("ya3ni"));
    }

    #[test]
    fn detection() {
        assert!(looks_like_arabizi("mar7aba") && has_arabizi_digit("mar7aba"));
        assert!(!has_arabizi_digit("hello"));
        assert!(!looks_like_arabizi("مرحبا"));
    }
}

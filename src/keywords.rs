//! Keyword and phrase matching on transcript text.
//!
//! Matching ignores case and collapses punctuation, so "Stand-by, cue 12!" matches the
//! phrase "standby cue 12" only when `whole_word` is off; "go" matches "Go." when it is on.

use crate::model::Keyword;
use serde::Serialize;
use uuid::Uuid;

/// A matched range in the original text, in byte offsets.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub keyword_id: Uuid,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

/// Find every occurrence of `needle` in `haystack`, ignoring case.
/// Returns byte ranges into `haystack`.
pub fn find_all(haystack: &str, needle: &str, whole_word: bool) -> Vec<(usize, usize)> {
    let needle = needle.trim();
    if needle.is_empty() {
        return vec![];
    }
    // Lowercase char-by-char and keep a map back to byte offsets in the original.
    let mut lower = String::with_capacity(haystack.len());
    let mut map: Vec<usize> = Vec::with_capacity(haystack.len() + 1);
    for (i, c) in haystack.char_indices() {
        for lc in c.to_lowercase() {
            let before = lower.len();
            lower.push(lc);
            for _ in before..lower.len() {
                map.push(i);
            }
        }
    }
    map.push(haystack.len());
    let needle_l = needle.to_lowercase();

    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(&needle_l) {
        let s = from + pos;
        let e = s + needle_l.len();
        let ok = !whole_word || {
            let before = lower[..s].chars().next_back();
            let after = lower[e..].chars().next();
            !before.map(is_word_char).unwrap_or(false) && !after.map(is_word_char).unwrap_or(false)
        };
        if ok {
            let os = map[s];
            // Map end: first original byte at or after e.
            let oe = if e >= map.len() - 1 { haystack.len() } else { map[e] };
            out.push((os, oe));
        }
        from = s + needle_l.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        if from >= lower.len() {
            break;
        }
    }
    out
}

pub fn contains(haystack: &str, needle: &str, whole_word: bool) -> bool {
    !find_all(haystack, needle, whole_word).is_empty()
}

/// Match a set of keywords against text. Overlapping matches keep the earliest, longest one.
pub fn match_keywords<'a>(text: &str, keywords: impl IntoIterator<Item = &'a Keyword>) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    for k in keywords {
        for (start, end) in find_all(text, &k.text, k.whole_word) {
            spans.push(Span { start, end, keyword_id: k.id });
        }
    }
    spans.sort_by(|a, b| a.start.cmp(&b.start).then((b.end - b.start).cmp(&(a.end - a.start))));
    let mut out: Vec<Span> = Vec::new();
    for s in spans {
        if out.last().map(|l| s.start < l.end).unwrap_or(false) {
            continue;
        }
        out.push(s);
    }
    out
}

/// Apply replacement and sensitive masking for display.
pub fn render_display<'a>(text: &str, keywords: impl IntoIterator<Item = &'a Keyword> + Clone) -> String {
    let spans = match_keywords(text, keywords.clone());
    if spans.is_empty() {
        return text.to_string();
    }
    let lookup: Vec<&Keyword> = keywords.into_iter().collect();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for s in spans {
        out.push_str(&text[last..s.start]);
        let k = lookup.iter().find(|k| k.id == s.keyword_id);
        let original = &text[s.start..s.end];
        match k {
            Some(k) if k.sensitive => out.extend(original.chars().map(|c| if c.is_whitespace() { c } else { '*' })),
            Some(k) if k.replacement.is_some() => out.push_str(k.replacement.as_deref().unwrap()),
            _ => out.push_str(original),
        }
        last = s.end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_whole_word() {
        assert_eq!(find_all("Standby, GO now", "go", true), vec![(9, 11)]);
        assert!(find_all("going home", "go", true).is_empty());
        assert_eq!(find_all("going home", "go", false), vec![(0, 2)]);
    }

    #[test]
    fn multi_word_phrase() {
        assert!(contains("Lights, standby cue 12 please", "standby cue 12", true));
        assert!(!contains("standby cue 123", "standby cue 12", true));
    }

    #[test]
    fn unicode_offsets() {
        let t = "Ça va? CUE prêt";
        let r = find_all(t, "cue", true);
        assert_eq!(&t[r[0].0..r[0].1], "CUE");
        let r = find_all(t, "prêt", true);
        assert_eq!(&t[r[0].0..r[0].1], "prêt");
    }

    #[test]
    fn repeated_matches() {
        assert_eq!(find_all("go go go", "go", true).len(), 3);
        assert_eq!(find_all("aaa", "aa", false).len(), 2);
    }

    #[test]
    fn overlap_keeps_longest() {
        let a = Keyword::new("cue");
        let b = Keyword::new("cue 12");
        let s = match_keywords("go cue 12", [&a, &b]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].keyword_id, b.id);
    }

    #[test]
    fn display_masking() {
        let mut bad = Keyword::new("darn it");
        bad.sensitive = true;
        let mut rep = Keyword::new("FOH");
        rep.replacement = Some("Front of House".into());
        let out = render_display("Darn it, tell foh now", [&bad, &rep]);
        assert_eq!(out, "**** **, tell Front of House now");
    }

    #[test]
    fn apostrophes_are_word_chars() {
        assert!(!contains("don't", "don", true));
    }
}

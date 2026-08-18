//! Matching for writing systems that do not delimit words.
//!
//! [`crate::tokenize`] splits on non-alphanumeric characters, which is the
//! whole story for Latin, Cyrillic, Greek, Hangul, and Devanagari. It is
//! useless for Burmese, Thai, Khmer, Lao, Chinese, and Japanese: those scripts
//! put spaces at phrase boundaries or nowhere at all, so a "word" arrives here
//! as a whole clause. Measured on a real public Burmese channel preview
//! (aggregate statistics only, never message text): a mean of 12.1 Myanmar
//! codepoints per whitespace token, max 33, where a Burmese word is 2–6. No
//! keyword can ever equal one of those tokens, so adding keywords in these
//! scripts to the word tables does nothing at all — see
//! docs/ENGINEERING_NOTES.md.
//!
//! The strategy here is **substring matching restricted to script runs, with
//! a cluster-boundary check**. Text is split into maximal runs of one
//! unsegmented script; a keyword may match anywhere inside a run of its own
//! script, provided the match does not start or end in the middle of a
//! grapheme cluster. That rejects the large and cheap class of false hits
//! where a keyword is the visual prefix of a longer syllable — Burmese ရေ
//! ("water") inside ရေး ("write") — without a dictionary, a model file, or a
//! dependency.
//!
//! Full syllable segmentation was considered and rejected: a syllable
//! segmenter would still be matching a *sequence* of syllables, which is what
//! a cluster-boundary-checked substring already is, and it would cost a
//! per-script rule set for no additional precision.
//!
//! What this deliberately does not fix: a keyword that straddles two real
//! words still matches (Chinese 中国 inside 美中国际, "US-China
//! international"). That inflates a count rather than inventing a place, and
//! a match still requires a place *and* a topic; see docs/DATA_MODEL.md.
//!
//! Privacy: everything here borrows the caller's text for the duration of one
//! call. Nothing in this module owns, buffers, or returns any part of a
//! message — only an index into a caller-owned table.

use std::collections::HashMap;

/// A writing system that does not delimit words, as far as this crate cares.
///
/// Japanese and Chinese share one class because Japanese mixes Han and kana
/// inside a single word (山火事, ストライキ), so splitting them would cut runs
/// in the middle of the very keywords being matched.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum ScriptClass {
    Myanmar,
    Thai,
    Lao,
    Khmer,
    /// Han ideographs plus Japanese kana.
    Cjk,
}

/// A maximal run of one script class. Borrowed from the caller's text.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Run<'a> {
    pub class: ScriptClass,
    pub text: &'a str,
}

/// Which unsegmented script `c` belongs to, if any.
///
/// Punctuation is deliberately excluded from every range — Burmese ၊ ။, Thai ๏
/// ๚, Khmer ។ — so that sentence punctuation ends a run and no keyword can
/// match across it.
pub fn class_of(c: char) -> Option<ScriptClass> {
    let c = c as u32;
    match c {
        // Myanmar: letters, signs, and digits, minus 104A..=104F punctuation.
        0x1000..=0x1049 | 0x1050..=0x109D => Some(ScriptClass::Myanmar),
        // Thai: minus 0E3F (currency) and 0E4F/0E5A/0E5B (punctuation).
        0x0E01..=0x0E3A | 0x0E40..=0x0E4E | 0x0E50..=0x0E59 => Some(ScriptClass::Thai),
        0x0E81..=0x0EBD | 0x0EC0..=0x0ECD | 0x0ED0..=0x0ED9 | 0x0EDC..=0x0EDF => {
            Some(ScriptClass::Lao)
        }
        // Khmer: minus 17D4..=17DB punctuation and currency.
        0x1780..=0x17D3 | 0x17DC..=0x17DD | 0x17E0..=0x17E9 => Some(ScriptClass::Khmer),
        // Kana, minus 30FB (the ・ used to separate words) and the
        // 3000..=303F CJK punctuation block.
        0x3041..=0x30FA | 0x30FC..=0x30FF => Some(ScriptClass::Cjk),
        // Han: BMP, extension A, compatibility, and extension B.
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x2_0000..=0x2_A6DF => {
            Some(ScriptClass::Cjk)
        }
        _ => None,
    }
}

/// True if `c` continues the grapheme cluster started by the character before
/// it: a vowel sign, tone mark, medial, or virama.
///
/// A match may not begin on one of these (it would start mid-syllable) and may
/// not be immediately followed by one (the last consonant of the match would
/// actually carry a vowel the keyword does not have, making it a different
/// word).
pub fn is_cluster_extender(class: ScriptClass, c: char) -> bool {
    let c = c as u32;
    match class {
        ScriptClass::Myanmar => matches!(c,
            0x102B..=0x103E
            | 0x1056..=0x1059
            | 0x105E..=0x1060
            | 0x1062..=0x1064
            | 0x1067..=0x106D
            | 0x1071..=0x1074
            | 0x1082..=0x108D
            | 0x109A..=0x109D),
        ScriptClass::Thai => matches!(c, 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E),
        ScriptClass::Lao => matches!(c, 0x0EB1 | 0x0EB4..=0x0EBC | 0x0EC8..=0x0ECD),
        ScriptClass::Khmer => matches!(c, 0x17B4..=0x17D3 | 0x17DD),
        // Kana voicing marks are the only combining characters in this class.
        ScriptClass::Cjk => matches!(c, 0x3099..=0x309C),
    }
}

/// True if `c` binds the character **after** it into its own cluster, so a
/// match starting just past it is starting mid-stack.
///
/// Only the two subjoining viraman signs qualify. Burmese asat (U+103A) and
/// the medials bind backwards, so they are extenders and nothing more.
fn is_forward_binder(class: ScriptClass, c: char) -> bool {
    match class {
        ScriptClass::Myanmar => c == '\u{1039}',
        ScriptClass::Khmer => c == '\u{17D2}',
        _ => false,
    }
}

/// Split `text` into maximal runs of one unsegmented script.
///
/// Latin text produces an empty vector without allocating, which is the
/// overwhelmingly common case on both live streams.
pub fn runs(text: &str) -> Vec<Run<'_>> {
    let mut out = Vec::new();
    let mut open: Option<(usize, ScriptClass)> = None;
    for (i, c) in text.char_indices() {
        match (open, class_of(c)) {
            (Some((start, prev)), Some(current)) if prev != current => {
                out.push(Run {
                    class: prev,
                    text: &text[start..i],
                });
                open = Some((i, current));
            }
            (Some((start, prev)), None) => {
                out.push(Run {
                    class: prev,
                    text: &text[start..i],
                });
                open = None;
            }
            (None, Some(current)) => open = Some((i, current)),
            _ => {}
        }
    }
    if let Some((start, class)) = open {
        out.push(Run {
            class,
            text: &text[start..],
        });
    }
    out
}

/// The single class every character of `keyword` shares, or `None` if the
/// keyword is empty or mixes scripts.
pub fn keyword_class(keyword: &str) -> Option<ScriptClass> {
    let mut chars = keyword.chars();
    let class = class_of(chars.next()?)?;
    if chars.any(|c| class_of(c) != Some(class)) {
        return None;
    }
    Some(class)
}

/// Substring lookup over script runs, shared by the place and topic tables.
///
/// Keywords are bucketed by (class, first character), and each bucket is kept
/// longest-first, so matching is leftmost-longest exactly like
/// [`crate::find_window`] — the two paths cannot disagree about precedence.
#[derive(Default)]
pub struct ScriptMatcher {
    by_first: HashMap<(ScriptClass, char), Vec<(String, usize)>>,
}

impl ScriptMatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `keyword` as resolving to `payload`.
    ///
    /// A keyword whose characters are not all one unsegmented script is
    /// ignored; `script_keywords_are_single_class` in each table's tests fails
    /// loudly rather than letting that happen silently.
    pub fn insert(&mut self, keyword: &str, payload: usize) {
        let Some(class) = keyword_class(keyword) else {
            return;
        };
        let first = keyword.chars().next().expect("non-empty, class matched");
        let bucket = self.by_first.entry((class, first)).or_default();
        if bucket.iter().any(|(k, _)| k == keyword) {
            return;
        }
        bucket.push((keyword.to_owned(), payload));
        bucket.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }

    pub fn is_empty(&self) -> bool {
        self.by_first.is_empty()
    }

    /// First keyword mentioned in `runs`, scanning each run left to right and
    /// preferring the longest keyword at a given position.
    pub fn find(&self, runs: &[Run<'_>]) -> Option<usize> {
        for run in runs {
            for (offset, c) in run.text.char_indices() {
                if is_cluster_extender(run.class, c) {
                    continue;
                }
                let Some(bucket) = self.by_first.get(&(run.class, c)) else {
                    continue;
                };
                let rest = &run.text[offset..];
                let preceded_by_binder = run.text[..offset]
                    .chars()
                    .next_back()
                    .is_some_and(|p| is_forward_binder(run.class, p));
                if preceded_by_binder {
                    continue;
                }
                for (keyword, payload) in bucket {
                    if !rest.starts_with(keyword.as_str()) {
                        continue;
                    }
                    let tail = &rest[keyword.len()..];
                    if tail
                        .chars()
                        .next()
                        .is_some_and(|n| is_cluster_extender(run.class, n))
                    {
                        continue;
                    }
                    return Some(*payload);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(text: &str) -> Vec<(ScriptClass, &str)> {
        runs(text).into_iter().map(|r| (r.class, r.text)).collect()
    }

    #[test]
    fn latin_text_produces_no_runs() {
        assert!(runs("protest in Kyiv today").is_empty());
        assert!(runs("").is_empty());
        // Hangul and Cyrillic delimit words already and are not our problem.
        assert!(runs("서울 시위").is_empty());
        assert!(runs("протест в Киеве").is_empty());
    }

    #[test]
    fn runs_split_on_script_change_and_on_punctuation() {
        // Burmese, then Latin, then Burmese again.
        assert_eq!(
            classes("ရန်ကုန် Yangon ငလျင်"),
            vec![
                (ScriptClass::Myanmar, "ရန်ကုန်"),
                (ScriptClass::Myanmar, "ငလျင်"),
            ]
        );
        // Burmese section mark ends a run, so nothing matches across it.
        assert_eq!(
            classes("ငလျင်။ရန်ကုန်"),
            vec![
                (ScriptClass::Myanmar, "ငလျင်"),
                (ScriptClass::Myanmar, "ရန်ကုန်"),
            ]
        );
        // Japanese Han and kana stay in one run; the ・ separator splits.
        assert_eq!(
            classes("東京・地震のデモ"),
            vec![(ScriptClass::Cjk, "東京"), (ScriptClass::Cjk, "地震のデモ")]
        );
        // Thai and Lao are adjacent blocks and must not merge.
        assert_eq!(
            classes("ไทยລາວ"),
            vec![(ScriptClass::Thai, "ไทย"), (ScriptClass::Lao, "ລາວ")]
        );
    }

    fn matcher(keywords: &[&str]) -> ScriptMatcher {
        let mut m = ScriptMatcher::new();
        for (i, k) in keywords.iter().enumerate() {
            m.insert(k, i);
        }
        m
    }

    #[test]
    fn matches_a_keyword_inside_an_undelimited_run() {
        let m = matcher(&["ငလျင်"]);
        // "A strong earthquake struck" — the keyword is mid-clause.
        assert_eq!(m.find(&runs("မြန်မာနိုင်ငံမှာ ငလျင်လှုပ်")), Some(0));
        assert_eq!(m.find(&runs("no myanmar here")), None);
    }

    /// The cluster-boundary rule, which is the entire reason this is not a
    /// plain `contains`. Burmese ရေ ("water") is the visual prefix of ရေး
    /// ("write"); a trailing vowel sign means a different word.
    #[test]
    fn a_trailing_vowel_sign_rejects_the_match() {
        let m = matcher(&["ရေ"]);
        assert_eq!(m.find(&runs("ရေ")), Some(0));
        assert_eq!(m.find(&runs("ရေး")), None, "ရေး is not ရေ");
        // Thai: ไฟ ("fire") must not match inside ไฟ้ with a tone mark.
        let thai = matcher(&["ไฟ"]);
        assert_eq!(thai.find(&runs("ไฟ")), Some(0));
        assert_eq!(thai.find(&runs("ไฟ้")), None);
    }

    /// A match may not start just after a subjoining virama, because that
    /// consonant is stacked under the previous one and belongs to its cluster.
    #[test]
    fn a_subjoined_consonant_does_not_start_a_match() {
        // ဒ preceded by the Myanmar virama U+1039 is stacked, not initial.
        // (A following letter is fine; a following vowel sign would be caught
        // by the trailing-extender rule instead.)
        let m = matcher(&["ဒ"]);
        assert_eq!(m.find(&runs("ဒဂုံ")), Some(0));
        assert_eq!(m.find(&runs("ဆန္ဒ")), None, "ဒ is subjoined here");
        // Khmer coeng behaves the same way.
        let khmer = matcher(&["ព"]);
        assert_eq!(khmer.find(&runs("ពន")), Some(0));
        assert_eq!(khmer.find(&runs("ភ្ព")), None);
    }

    #[test]
    fn leftmost_then_longest_wins() {
        // 東京 at position 0 beats 京都 starting one character later.
        let m = matcher(&["京都", "東京"]);
        assert_eq!(m.find(&runs("東京都")), Some(1));
        // At the same position, the longer keyword wins.
        let n = matcher(&["山火", "山火事"]);
        assert_eq!(n.find(&runs("山火事が発生")), Some(1));
    }

    #[test]
    fn mixed_script_keywords_are_ignored_rather_than_half_registered() {
        let m = matcher(&["東京tokyo", "", "plain"]);
        assert!(m.is_empty());
    }
}

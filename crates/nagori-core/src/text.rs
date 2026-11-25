use unicode_normalization::char::canonical_combining_class;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};

/// Canonical text normalization.
///
/// Shared by `EntryFactory`, the capture loop, the search backend, and tests.
/// Keep all callers on this single function so `SearchDocument::normalized_text`
/// stays consistent across crates.
pub fn normalize_text(input: &str) -> String {
    // Single pass over the NFKC-folded, lowercased char stream: collapse runs
    // of whitespace to one space and trim the ends, building the result in one
    // allocation. The previous `nfkc().collect().to_lowercase().split_whitespace()
    // .join(" ")` chain made three intermediate `String`s / a `Vec` per call,
    // which is paid on every capture against bodies up to 512 KiB.
    let mut out = String::with_capacity(input.len());
    let mut wrote_any = false;
    let mut pending_space = false;
    for ch in input.nfkc().flat_map(char::to_lowercase) {
        if ch.is_whitespace() {
            // Defer the separator: only emit it once a non-space char follows,
            // so leading and trailing runs are trimmed and internal runs
            // collapse to a single space.
            pending_space = wrote_any;
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
            wrote_any = true;
        }
    }
    out
}

/// Whether [`normalize_text`] treats the char boundary `at` as a seam.
///
/// At a seam the normalized `text[..at]` and `text[at..]` read the same as
/// the normalized `text` on either side of it, apart from how whitespace at
/// the seam is collapsed and trimmed.
///
/// That holds at the ends of the text and before a char that is NFKC-stable
/// with canonical combining class 0. Such a char never composes with what
/// precedes it, and combining marks are never reordered across it, so nothing
/// on one side can change how the other side normalizes. A combining mark
/// (`U+3099` after `か` becomes `が`) or a compatibility char that decomposes
/// into one (half-width `ﾞ`) does not qualify.
#[must_use]
pub fn is_normalization_boundary(text: &str, at: usize) -> bool {
    if at == 0 {
        return true;
    }
    text.get(at..).is_some_and(|rest| {
        rest.chars().next().is_none_or(|ch| {
            canonical_combining_class(ch) == 0
                && is_nfkc_quick(std::iter::once(ch)) == IsNormalized::Yes
        })
    })
}

/// Returns `true` if `input` contains any CJK character.
///
/// Recognizes Hiragana, Katakana (incl. phonetic extensions), the CJK symbols
/// block (`々`, `〆`, `〇`, …), Bopomofo, Hangul (Jamo, compatibility Jamo, and
/// syllables), and CJK ideographs — including Extension A–J, the compatibility
/// blocks, and the supplementary-plane extensions.
///
/// Lives in core (next to [`normalize_text`]) because the search plan dispatch
/// in [`crate::services::search`] needs this classification to decide when
/// ngram fan-out is worthwhile, and core cannot depend on `nagori-search`.
/// `nagori-search` re-exports it so existing call sites keep their import path.
///
/// The range set must stay broad: it gates whether the Auto/Hybrid plan runs
/// ngram at all, so a CJK char it fails to recognize would silently disable
/// ngram recall for that query. The document side (`generate_document_ngrams`)
/// indexes every non-whitespace char regardless of script, so any narrower
/// definition here would desync the query gate from what is actually indexed.
/// Recognizing a non-CJK char (e.g. ideographic punctuation) only costs an
/// ngram fan-out that finds nothing, so the ranges err toward inclusion.
#[must_use]
pub fn has_cjk(input: &str) -> bool {
    input.chars().any(|ch| {
        matches!(
            ch as u32,
            0x1100..=0x11ff      // Hangul Jamo
            | 0x3000..=0x303f    // CJK Symbols and Punctuation (々 〆 〇 …)
            | 0x3040..=0x30ff    // Hiragana + Katakana
            | 0x3100..=0x312f    // Bopomofo
            | 0x3130..=0x318f    // Hangul Compatibility Jamo
            | 0x31a0..=0x31bf    // Bopomofo Extended
            | 0x31f0..=0x31ff    // Katakana Phonetic Extensions
            | 0x3400..=0x4dbf    // CJK Unified Ideographs Extension A
            | 0x4e00..=0x9fff    // CJK Unified Ideographs
            | 0xac00..=0xd7af    // Hangul Syllables
            | 0xf900..=0xfaff    // CJK Compatibility Ideographs
            | 0x20000..=0x2ee5d  // CJK Unified Ideographs Extension B–F + I
            | 0x2f800..=0x2fa1f  // CJK Compatibility Ideographs Supplement
            | 0x30000..=0x33479  // CJK Unified Ideographs Extension G + H + J
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_boundaries_split_text_without_changing_its_normalization() {
        let text = "aか\u{3099}ｶﾞ漢字 x";
        let boundaries: Vec<usize> = (0..=text.len())
            .filter(|&at| text.is_char_boundary(at) && is_normalization_boundary(text, at))
            .collect();
        // Not before the combining U+3099, nor before the half-width kana,
        // which are compatibility chars (the voiced mark decomposes to U+3099).
        assert_eq!(boundaries, vec![0, 1, 13, 16, 19, 20, 21], "{boundaries:?}");
        let whole = normalize_text(text);
        for at in boundaries {
            let joined = normalize_text(&text[..at]) + &normalize_text(&text[at..]);
            assert_eq!(
                joined.replace(' ', ""),
                whole.replace(' ', ""),
                "split at {at}"
            );
        }
    }

    #[test]
    fn normalizes_width_and_case() {
        assert_eq!(normalize_text("ＡＢＣ  １２３"), "abc 123");
    }

    #[test]
    fn collapses_internal_whitespace() {
        assert_eq!(normalize_text("hello\n\tworld   foo"), "hello world foo");
    }

    #[test]
    fn detects_cjk_scripts() {
        assert!(has_cjk("検索"));
        assert!(has_cjk("クリップ"));
        assert!(has_cjk("alpha 設計"));
        assert!(has_cjk("한글"));
        assert!(!has_cjk("needle"));
        assert!(!has_cjk("github.com/path"));
    }

    #[test]
    fn detects_rare_and_supplementary_cjk() {
        // These gate Auto/Hybrid ngram, so they must be recognized even though
        // they fall outside the common BMP ideograph block.
        assert!(has_cjk("豈"), "CJK Compatibility Ideograph U+F900");
        assert!(has_cjk("𠀋"), "CJK Extension B U+2000B");
        assert!(has_cjk("𪜀"), "CJK Extension C U+2A700");
    }

    #[test]
    fn detects_cjk_symbols_and_jamo_and_bopomofo() {
        // The document side indexes these as ngrams regardless of script, so
        // the query gate must recognize them too or recall silently breaks.
        assert!(has_cjk("々"), "CJK iteration mark U+3005");
        assert!(has_cjk("〆"), "CJK symbol U+3006");
        assert!(has_cjk("ㄅ"), "Bopomofo U+3105");
        assert!(has_cjk("ㄱ"), "Hangul Compatibility Jamo U+3131");
        assert!(has_cjk("ᄀ"), "Hangul Jamo U+1100");
        assert!(has_cjk("ㇰ"), "Katakana Phonetic Extension U+31F0");
    }
}

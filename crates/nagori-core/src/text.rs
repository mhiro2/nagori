use unicode_normalization::UnicodeNormalization;

/// Canonical text normalization.
///
/// Shared by `EntryFactory`, the capture loop, the search backend, and tests.
/// Keep all callers on this single function so `SearchDocument::normalized_text`
/// stays consistent across crates.
pub fn normalize_text(input: &str) -> String {
    input
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns `true` if `input` contains any CJK character.
///
/// Recognizes Hiragana, Katakana, Hangul syllables, and CJK ideographs —
/// including Extension A–J, the compatibility blocks, and the
/// supplementary-plane extensions.
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
#[must_use]
pub fn has_cjk(input: &str) -> bool {
    input.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3040..=0x30ff      // Hiragana + Katakana
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
}

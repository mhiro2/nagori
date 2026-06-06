// `has_cjk` now lives in `nagori_core::text` (search plan dispatch in core
// needs it and core cannot depend on this crate). Re-exported via `lib.rs` so
// existing `nagori_search::has_cjk` call sites keep compiling.

/// Hard cap on how many leading non-whitespace characters a single document
/// contributes to the ngram index.
///
/// Each entry's stored text can be up to `max_entry_size_bytes` (default
/// 512KB), and a 2/3-gram index over the full body would emit ~2N rows per
/// entry. For a 524KB UTF-8 paste that is ~1M ngram rows for a single row
/// in `entries`, which inflates the `SQLite` file and slows fan-out queries.
/// Capping at 4096 chars covers virtually every clipboard search scenario
/// — fuzzy matches against the head of the document — while keeping the
/// per-entry index size bounded.
pub const MAX_NGRAM_INPUT_CHARS: usize = 4096;

/// Fold a single Katakana scalar to its Hiragana counterpart.
///
/// The Katakana block `U+30A1..=U+30F6` is laid out parallel to Hiragana with a
/// fixed `+0x60` offset, so this also folds the small kana, `ヰ/ヱ`, and `ヵ/ヶ`
/// onto their Hiragana forms. Katakana iteration marks fold to the Hiragana
/// ones. Everything else — including the prolonged sound mark `ー` (U+30FC),
/// the middle dot `・` (U+30FB), `゠` (U+30A0), and the rare `ヷヸヹヺ`
/// (U+30F7..=U+30FA, which have no single-scalar Hiragana form) — passes
/// through unchanged.
///
/// Folding is applied identically to document and query grams (see
/// [`generate_document_ngrams`] / [`generate_query_ngrams`]), so a Katakana
/// clip recalls against a Hiragana query and vice versa without touching the
/// stored `normalized_text`, the FTS index, or the semantic embedding input.
fn fold_kana(ch: char) -> char {
    match ch as u32 {
        cp @ 0x30A1..=0x30F6 => char::from_u32(cp - 0x60).unwrap_or(ch),
        0x30FD => 'ゝ',
        0x30FE => 'ゞ',
        _ => ch,
    }
}

/// Whether a single character is a Han ideograph worth indexing as a 1-gram.
///
/// Restricted to Han ideographs on purpose. A lone ideograph is a meaningful
/// query unit, but a kana 1-gram would own a near-universal posting list
/// (`の`/`は`/…) — made worse by the kana fold collapsing Katakana onto
/// Hiragana — and Hangul / kana single-char recall is rare enough to defer.
/// This is intentionally narrower than [`nagori_core::has_cjk`], whose range
/// also matches `ー`/`・`/`゠` and the kana blocks.
const fn is_han_unigram_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4dbf      // CJK Unified Ideographs Extension A
        | 0x4e00..=0x9fff    // CJK Unified Ideographs
        | 0xf900..=0xfaff    // CJK Compatibility Ideographs
        | 0x20000..=0x2ee5d  // CJK Unified Ideographs Extension B–F + I
        | 0x2f800..=0x2fa1f  // CJK Compatibility Ideographs Supplement
        | 0x30000..=0x33479  // CJK Unified Ideographs Extension G + H + J
    )
}

/// Collect the kana-folded, whitespace-stripped, cap-bounded character window a
/// gram generator works over. Shared by the document and query generators so
/// both see the same canonical form.
fn gram_chars(input: &str) -> Vec<char> {
    input
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .take(MAX_NGRAM_INPUT_CHARS)
        .map(fold_kana)
        .collect()
}

fn push_window_grams(chars: &[char], grams: &mut Vec<String>) {
    for size in [2, 3] {
        if chars.len() < size {
            continue;
        }
        for window in chars.windows(size) {
            grams.push(window.iter().collect());
        }
    }
}

/// Grams indexed for a stored document: the 2/3-gram set plus a 1-gram for
/// every Han ideograph.
///
/// The Han 1-grams let a lone-ideograph query (`検`) recall entries beyond the
/// bounded substring window — `unicode61` FTS collapses a CJK run to a single
/// token, so without them a single kanji only matches via the recent-window
/// LIKE scan. See [`is_han_unigram_char`] for why the 1-grams stop at Han.
pub fn generate_document_ngrams(input: &str) -> Vec<String> {
    let chars = gram_chars(input);
    let mut grams = Vec::new();
    for &ch in &chars {
        if is_han_unigram_char(ch) {
            grams.push(ch.to_string());
        }
    }
    push_window_grams(&chars, &mut grams);
    grams.sort();
    grams.dedup();
    grams
}

/// Grams used to probe the index for a query.
///
/// A single Han ideograph yields that 1-gram (matching the document side); any
/// other single character yields nothing (kana/ASCII 1-grams are not indexed).
/// For 2+ characters we keep only the 2/3-gram set: mixing in single-char Han
/// grams would change the overlap denominator (`検索` would go from 1 gram to
/// 3) and let entries that merely share one ideograph surface as noise.
pub fn generate_query_ngrams(input: &str) -> Vec<String> {
    let chars = gram_chars(input);
    if chars.len() == 1 {
        return if is_han_unigram_char(chars[0]) {
            vec![chars[0].to_string()]
        } else {
            Vec::new()
        };
    }
    let mut grams = Vec::new();
    push_window_grams(&chars, &mut grams);
    grams.sort();
    grams.dedup();
    grams
}

/// Returns `true` if [`generate_document_ngrams`] would discard tail characters
/// from `input` because the non-whitespace character count exceeds
/// [`MAX_NGRAM_INPUT_CHARS`].
///
/// Callers that index user-supplied text can pair this with an audit-event
/// write so a clipboard entry larger than the ngram budget leaves a
/// breadcrumb explaining why fuzzy search misses bytes past the cap. The
/// function short-circuits as soon as the cap is exceeded so it stays O(N)
/// in the worst case but typically `O(MAX_NGRAM_INPUT_CHARS)`. Kana folding
/// never changes the non-whitespace character count, so the detector stays in
/// sync with the generator without folding here.
#[must_use]
pub fn ngram_input_was_truncated(input: &str) -> bool {
    input
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .nth(MAX_NGRAM_INPUT_CHARS)
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_grams_fold_katakana_to_hiragana() {
        // The document side folds Katakana onto Hiragana, so a Katakana clip is
        // indexed under the same grams a Hiragana query produces.
        let grams = generate_document_ngrams("クリップ");
        assert!(grams.contains(&"くり".to_owned()));
        assert!(grams.contains(&"りっ".to_owned()));
        assert!(grams.contains(&"くりっ".to_owned()));
        // No raw Katakana grams survive the fold.
        assert!(!grams.iter().any(|g| g.contains('ク')));
    }

    #[test]
    fn katakana_document_matches_hiragana_query_grams() {
        // The whole point of the fold: a Katakana document and a Hiragana query
        // share grams.
        let doc = generate_document_ngrams("クリップ");
        let query = generate_query_ngrams("くりっぷ");
        assert!(!query.is_empty());
        assert!(query.iter().all(|g| doc.contains(g)));
    }

    #[test]
    fn document_grams_include_han_unigrams() {
        // Han ideographs get a 1-gram so a lone-kanji query can match; the kana
        // around them do not.
        let grams = generate_document_ngrams("検索エンジン");
        assert!(grams.contains(&"検".to_owned()));
        assert!(grams.contains(&"索".to_owned()));
        assert!(!grams.contains(&"エ".to_owned()));
    }

    #[test]
    fn single_han_query_yields_unigram() {
        assert_eq!(generate_query_ngrams("検"), vec!["検".to_owned()]);
    }

    #[test]
    fn single_kana_or_ascii_query_yields_nothing() {
        // Only Han ideographs are 1-gram query units; a lone kana or ASCII char
        // falls through to the substring path instead.
        assert!(generate_query_ngrams("あ").is_empty());
        assert!(generate_query_ngrams("n").is_empty());
    }

    #[test]
    fn multi_char_query_keeps_only_window_grams() {
        // A 2+ char query must not gain single-char Han grams, so the overlap
        // denominator stays the window-gram count.
        let grams = generate_query_ngrams("検索");
        assert_eq!(grams, vec!["検索".to_owned()]);
        assert!(!grams.contains(&"検".to_owned()));
    }

    #[test]
    fn caps_input_to_max_chars() {
        // Build an input longer than the cap. After truncation we should get
        // 2 + 3 grams up to MAX_NGRAM_INPUT_CHARS, never the full corpus.
        let body: String = "a".repeat(MAX_NGRAM_INPUT_CHARS * 2);
        let grams = generate_document_ngrams(&body);
        // ASCII has no Han 1-grams, and for all-identical chars after dedup we
        // end up with exactly two distinct grams: "aa" and "aaa". The point is
        // just that we didn't allocate proportional to the full input length.
        assert_eq!(grams, vec!["aa".to_owned(), "aaa".to_owned()]);
    }

    #[test]
    fn truncation_detector_matches_generate_ngrams() {
        // Whitespace doesn't count toward the cap (it's filtered before the
        // `take`), so an input padded with spaces past the cap-byte budget
        // must still be reported as "not truncated".
        let padded: String = "a".repeat(MAX_NGRAM_INPUT_CHARS) + &" ".repeat(1024);
        assert!(
            !ngram_input_was_truncated(&padded),
            "trailing whitespace must not flip the truncation flag",
        );

        let exactly_at_cap: String = "a".repeat(MAX_NGRAM_INPUT_CHARS);
        assert!(
            !ngram_input_was_truncated(&exactly_at_cap),
            "exactly-at-cap input must not be flagged as truncated",
        );

        let over_cap: String = "a".repeat(MAX_NGRAM_INPUT_CHARS + 1);
        assert!(
            ngram_input_was_truncated(&over_cap),
            "one char past the cap must flip the truncation flag",
        );
    }
}

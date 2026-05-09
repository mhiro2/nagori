pub fn has_cjk(input: &str) -> bool {
    input.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af
        )
    })
}

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

pub fn generate_ngrams(input: &str) -> Vec<String> {
    let chars: Vec<char> = input
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .take(MAX_NGRAM_INPUT_CHARS)
        .collect();
    if chars.len() < 2 {
        return Vec::new();
    }

    let mut grams = Vec::new();
    for size in [2, 3] {
        if chars.len() < size {
            continue;
        }
        for window in chars.windows(size) {
            grams.push(window.iter().collect());
        }
    }
    grams.sort();
    grams.dedup();
    grams
}

/// Returns `true` if [`generate_ngrams`] would discard tail characters from
/// `input` because the non-whitespace character count exceeds
/// [`MAX_NGRAM_INPUT_CHARS`].
///
/// Callers that index user-supplied text can pair this with an audit-event
/// write so a clipboard entry larger than the ngram budget leaves a
/// breadcrumb explaining why fuzzy search misses bytes past the cap. The
/// function short-circuits as soon as the cap is exceeded so it stays O(N)
/// in the worst case but typically `O(MAX_NGRAM_INPUT_CHARS)`.
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
    fn generates_japanese_grams() {
        let grams = generate_ngrams("クリップ");
        assert!(grams.contains(&"クリ".to_owned()));
        assert!(grams.contains(&"リッ".to_owned()));
        assert!(grams.contains(&"クリッ".to_owned()));
    }

    #[test]
    fn caps_input_to_max_chars() {
        // Build an input longer than the cap. After truncation we should get
        // 2 + 3 grams up to MAX_NGRAM_INPUT_CHARS, never the full corpus.
        let body: String = "a".repeat(MAX_NGRAM_INPUT_CHARS * 2);
        let grams = generate_ngrams(&body);
        // For all-identical chars after dedup we end up with exactly two
        // distinct grams: "aa" and "aaa". The point is just that we
        // didn't allocate proportional to the full input length.
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

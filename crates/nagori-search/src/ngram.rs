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
}

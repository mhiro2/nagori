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
}

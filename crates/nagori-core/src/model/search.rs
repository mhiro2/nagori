use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::content::{ClipboardContent, ContentKind};
use super::{ClipboardEntry, EntryId, Sensitivity};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchDocument {
    pub entry_id: EntryId,
    pub title: Option<String>,
    pub preview: String,
    pub normalized_text: String,
    pub tokens: Vec<String>,
    pub language: Option<String>,
}

impl SearchDocument {
    pub fn new(entry_id: EntryId, content: &ClipboardContent, normalized_text: String) -> Self {
        let plain = content.plain_text().unwrap_or_default();
        let preview = make_preview(plain, 180);
        let title = match content {
            ClipboardContent::Url(value) => value.domain.clone(),
            ClipboardContent::Code(value) => value.language_hint.clone(),
            _ => None,
        };
        let tokens = normalized_text
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect();
        Self {
            entry_id,
            title,
            preview,
            normalized_text,
            tokens,
            language: match content {
                ClipboardContent::Code(value) => value.language_hint.clone(),
                _ => None,
            },
        }
    }
}

/// True iff `keyword` occurs in `text` with a non-word ASCII boundary on the
/// left and ASCII whitespace immediately on the right. Used by the code
/// heuristic so URL path segments like `/function/docs` and identifiers like
/// `somefn` do not match `fn` / `function`.
pub(crate) fn keyword_followed_by_whitespace(text: &str, keyword: &str) -> bool {
    if keyword.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    let kw_len = keyword.len();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(keyword) {
        let start = search_from + rel;
        let end = start + kw_len;
        // Left boundary checks the preceding *char*, not the preceding byte:
        // inspecting `bytes[start - 1]` would read a UTF-8 continuation byte
        // as a non-word boundary, so `あfn x` would wrongly look like code.
        let left_ok = text[..start]
            .chars()
            .next_back()
            .is_none_or(|prev| !is_word_char(prev));
        let right_ok = end < bytes.len() && bytes[end].is_ascii_whitespace();
        if left_ok && right_ok {
            return true;
        }
        // Advance by the first char's byte length so `search_from` always
        // lands on a char boundary even when `keyword` starts with a
        // multi-byte char (current callers only pass ASCII keywords).
        search_from = start + keyword.chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Build a whitespace-compacted preview of `text`, capped at `max_chars`.
///
/// When truncation occurs the trailing `…` is counted toward `max_chars`, so the
/// returned string is always `<= max_chars` Unicode scalar values.
pub fn make_preview(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    if max_chars == 0 {
        return String::new();
    }
    let take_n = max_chars - 1;
    let mut preview: String = compact.chars().take(take_n).collect();
    preview.push('…');
    preview
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub raw: String,
    pub normalized: String,
    pub mode: SearchMode,
    pub limit: usize,
    pub filters: SearchFilters,
    pub recent_order: crate::settings::RecentOrder,
}

impl SearchQuery {
    pub fn new(raw: impl Into<String>, normalized: impl Into<String>, limit: usize) -> Self {
        Self {
            raw: raw.into(),
            normalized: normalized.into(),
            mode: SearchMode::Auto,
            limit,
            filters: SearchFilters::default(),
            recent_order: crate::settings::RecentOrder::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchMode {
    Auto,
    Recent,
    Exact,
    Fuzzy,
    FullText,
    Semantic,
}

/// Search-time projection of a stored entry: exactly the fields the ranker
/// scores on plus what a [`SearchResult`] carries — nothing else.
///
/// Candidate fetches used to return full [`ClipboardEntry`] values, which
/// meant deserialising and carrying up to 512 KiB of body per candidate
/// (`limit × 8` per branch, three branches) on every keystroke even though
/// ranking never reads the content. Providers project rows into this type
/// instead so the per-candidate payload stays proportional to what ranking
/// actually consumes (`normalized_text` is needed for the substring / exact
/// checks and is the one potentially large field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCandidate {
    pub entry_id: EntryId,
    /// Same value as [`SearchDocument::normalized_text`].
    pub normalized_text: String,
    pub preview: String,
    /// Canonical language id for `Code` rows; see [`SearchResult::language`].
    pub language: Option<String>,
    pub content_kind: ContentKind,
    pub created_at: OffsetDateTime,
    pub use_count: u32,
    pub pinned: bool,
    pub sensitivity: Sensitivity,
    pub source_app_name: Option<String>,
    /// Pixel dimensions for `Image` rows; see [`SearchResult::image_width`].
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
}

impl SearchCandidate {
    /// Project a full entry down to its rankable fields. Storage backends
    /// project at the SQL layer instead; this is for in-memory providers and
    /// tests that start from a [`ClipboardEntry`].
    #[must_use]
    pub fn from_entry(entry: &ClipboardEntry) -> Self {
        let (image_width, image_height) = match &entry.content {
            ClipboardContent::Image(image) => (image.width, image.height),
            _ => (None, None),
        };
        Self {
            entry_id: entry.id,
            normalized_text: entry.search.normalized_text.clone(),
            preview: entry.search.preview.clone(),
            language: entry.search.language.clone(),
            content_kind: entry.content_kind(),
            created_at: entry.metadata.created_at,
            use_count: entry.metadata.use_count,
            pinned: entry.lifecycle.pinned,
            sensitivity: entry.sensitivity,
            source_app_name: entry
                .metadata
                .source
                .as_ref()
                .and_then(|source| source.name.clone()),
            image_width,
            image_height,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchFilters {
    pub kinds: Vec<ContentKind>,
    pub pinned_only: bool,
    pub source_app: Option<String>,
    pub created_after: Option<OffsetDateTime>,
    pub created_before: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub entry_id: EntryId,
    pub score: f32,
    pub rank_reason: Vec<RankReason>,
    pub preview: String,
    pub content_kind: ContentKind,
    pub created_at: OffsetDateTime,
    pub pinned: bool,
    pub sensitivity: Sensitivity,
    /// Display name of the app the clip was captured from, when known.
    /// Carried through so search result rows show the same source label as
    /// `EntryDto` does — without this, opening the palette and typing a
    /// query removes the "from 1Password" hint that recent listing shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    /// Canonical language id (`json`, `rust`, …) for `Code` rows, mirrored
    /// from [`SearchDocument::language`]. `None` for non-code rows and for
    /// legacy code rows captured before language detection landed; the
    /// result row falls back to a client-side sniff in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Pixel dimensions for `Image` rows, when a header probe captured them.
    /// `None` for non-image rows and for images captured before the probe
    /// landed. The two are populated together or not at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_height: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RankReason {
    ExactMatch,
    PrefixMatch,
    SubstringMatch,
    FullTextMatch,
    NgramMatch,
    SemanticMatch,
    Recent,
    FrequentlyUsed,
    Pinned,
}

#[cfg(test)]
mod tests {
    use super::{keyword_followed_by_whitespace, make_preview};

    #[test]
    fn keyword_match_needs_whitespace_on_the_right() {
        // The bare keyword at the start of the text with a following space is
        // the canonical "this looks like code" signal.
        assert!(keyword_followed_by_whitespace("fn main()", "fn"));
        assert!(keyword_followed_by_whitespace("function call", "function"));
    }

    #[test]
    fn keyword_at_end_without_trailing_whitespace_does_not_match() {
        // `right_ok` requires a byte after the keyword, so a keyword that ends
        // the string never qualifies — it has no whitespace boundary.
        assert!(!keyword_followed_by_whitespace("use fn", "fn"));
    }

    #[test]
    fn keyword_inside_an_identifier_does_not_match() {
        // Left boundary must be a non-word byte: `somefn` and `myfn` carry a
        // word byte immediately before the keyword.
        assert!(!keyword_followed_by_whitespace("somefn x", "fn"));
        // URL path segments like `/function/docs` end the keyword on `/`,
        // which is not whitespace, so the right boundary rejects them.
        assert!(!keyword_followed_by_whitespace(
            "/function/docs",
            "function"
        ));
    }

    #[test]
    fn scans_past_a_failed_match_to_a_later_valid_one() {
        // First "fn" sits inside "myfn" (word byte on the left); the loop must
        // keep scanning and accept the standalone "fn" later in the string.
        assert!(keyword_followed_by_whitespace("myfn fn x", "fn"));
    }

    #[test]
    fn punctuation_is_a_valid_left_boundary() {
        // A non-word byte such as `(` opens the left boundary even mid-string.
        assert!(keyword_followed_by_whitespace("x=(fn arg)", "fn"));
    }

    #[test]
    fn empty_keyword_never_matches() {
        assert!(!keyword_followed_by_whitespace("anything", ""));
        assert!(!keyword_followed_by_whitespace("", ""));
    }

    #[test]
    fn multibyte_char_before_keyword_is_a_word_boundary() {
        // A CJK letter immediately before the keyword is a word char, so the
        // keyword is part of a larger token and must not match. The left
        // boundary inspects the preceding char (not its trailing UTF-8 byte),
        // which would otherwise read as a non-word boundary.
        assert!(!keyword_followed_by_whitespace("あfn x", "fn"));
        // A multi-byte char as a clean left boundary (followed by the keyword
        // then whitespace) still works, and the scan must not panic when it
        // advances past a match that starts mid-string.
        assert!(keyword_followed_by_whitespace("　fn x", "fn"));
    }

    #[test]
    fn multibyte_keyword_does_not_panic_when_scanning() {
        // The first "検索" fails the right boundary ('x' follows), so the loop
        // advances and rescans. The advance must land on a char boundary even
        // though the keyword starts with a multi-byte char — otherwise the
        // next `text[search_from..]` slice panics mid-char. The standalone
        // second "検索" (followed by a space) is then accepted.
        assert!(keyword_followed_by_whitespace("検索x 検索 y", "検索"));
    }

    #[test]
    fn make_preview_caps_at_max_chars_counting_the_ellipsis() {
        let preview = make_preview("abcdef", 4);
        assert_eq!(preview, "abc…");
        assert_eq!(preview.chars().count(), 4);
    }

    #[test]
    fn make_preview_compacts_whitespace_without_truncating_short_text() {
        assert_eq!(make_preview("  a\t\n b  ", 180), "a b");
    }
}

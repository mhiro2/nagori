use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::content::{ClipboardContent, ContentKind};
use super::{EntryId, Sensitivity};

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
        let left_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let right_ok = end < bytes.len() && bytes[end].is_ascii_whitespace();
        if left_ok && right_ok {
            return true;
        }
        search_from = start + 1;
    }
    false
}

const fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
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

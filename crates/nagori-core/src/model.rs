use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryId(pub Uuid);

impl EntryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EntryId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for EntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for EntryId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Identifier used to detect clipboard changes between capture ticks.
///
/// Variants are explicitly typed so a native platform sequence number cannot
/// be confused with a content-hash fallback that happens to share the same
/// textual representation. Equality is by-variant: two `ContentHash` values
/// with identical hex strings are equal, but `Native(5)` and
/// `ContentHash("5")` are not.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClipboardSequence {
    /// Native platform sequence (e.g. macOS `NSPasteboard` `changeCount`).
    /// `i64` covers both 32- and 64-bit `NSInteger` ranges; wraparound is
    /// far outside any realistic per-process lifetime.
    Native(i64),
    /// SHA-256 hex of the clipboard payload, used when the platform exposes
    /// no native sequence counter.
    ContentHash(String),
    /// Sentinel for platforms that do not implement clipboard polling.
    Unsupported,
}

impl ClipboardSequence {
    /// Construct a `Native` sequence.
    pub const fn native(count: i64) -> Self {
        Self::Native(count)
    }

    /// Construct a `ContentHash` sequence from any string-like value.
    pub fn content_hash(value: impl Into<String>) -> Self {
        Self::ContentHash(value.into())
    }

    /// Construct an `Unsupported` sentinel.
    pub const fn unsupported() -> Self {
        Self::Unsupported
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub id: EntryId,
    pub content: ClipboardContent,
    pub metadata: EntryMetadata,
    pub search: SearchDocument,
    pub sensitivity: Sensitivity,
    pub lifecycle: EntryLifecycle,
}

impl ClipboardEntry {
    pub const fn content_kind(&self) -> ContentKind {
        self.content.kind()
    }

    pub fn plain_text(&self) -> Option<&str> {
        self.content.plain_text()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardContent {
    Text(TextContent),
    Url(UrlContent),
    Code(CodeContent),
    Image(ImageContent),
    FileList(FileListContent),
    RichText(RichTextContent),
    Unknown(UnknownContent),
}

impl ClipboardContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextContent::new(text.into()))
    }

    pub fn from_plain_text(text: impl Into<String>) -> Self {
        let text = text.into();
        if let Some(url) = UrlContent::parse(&text) {
            Self::Url(url)
        } else if CodeContent::looks_like_code(&text) {
            Self::Code(CodeContent::new(text, None))
        } else {
            Self::Text(TextContent::new(text))
        }
    }

    pub const fn kind(&self) -> ContentKind {
        match self {
            Self::Text(_) => ContentKind::Text,
            Self::Url(_) => ContentKind::Url,
            Self::Code(_) => ContentKind::Code,
            Self::Image(_) => ContentKind::Image,
            Self::FileList(_) => ContentKind::FileList,
            Self::RichText(_) => ContentKind::RichText,
            Self::Unknown(_) => ContentKind::Unknown,
        }
    }

    pub fn plain_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(&value.text),
            Self::Url(value) => Some(&value.raw),
            Self::Code(value) => Some(&value.text),
            Self::FileList(value) => Some(&value.display_text),
            Self::RichText(value) => Some(&value.plain_text),
            Self::Unknown(value) => value.plain_text.as_deref(),
            Self::Image(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextContent {
    pub text: String,
    pub char_count: usize,
    pub byte_count: usize,
    pub line_count: usize,
}

impl TextContent {
    pub fn new(text: String) -> Self {
        Self {
            char_count: text.chars().count(),
            byte_count: text.len(),
            line_count: text.lines().count().max(1),
            text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlContent {
    pub raw: String,
    pub normalized: String,
    pub domain: Option<String>,
}

impl UrlContent {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            return None;
        }
        let parsed = Url::parse(trimmed).ok()?;
        Some(Self {
            raw: raw.to_owned(),
            normalized: parsed.as_str().trim_end_matches('/').to_lowercase(),
            domain: parsed.domain().map(ToOwned::to_owned),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeContent {
    pub text: String,
    pub language_hint: Option<String>,
}

impl CodeContent {
    pub const fn new(text: String, language_hint: Option<String>) -> Self {
        Self {
            text,
            language_hint,
        }
    }

    pub fn looks_like_code(text: &str) -> bool {
        // Each keyword must be a whole token *followed by ASCII whitespace*
        // so a URL path segment like `/function/docs` does not trip the
        // heuristic — in real code these keywords are always followed by an
        // identifier (`fn foo`, `class Foo`), never by `/` or `?`.
        const KEYWORDS: &[&str] = &["fn", "function", "class", "package"];
        let trimmed = text.trim();
        if !trimmed.contains('\n') {
            return false;
        }
        if KEYWORDS
            .iter()
            .any(|kw| keyword_followed_by_whitespace(trimmed, kw))
        {
            return true;
        }
        trimmed.contains("=>")
            || trimmed.contains("#include")
            || (trimmed.contains('{') && trimmed.contains('}'))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    pub payload_ref: PayloadRef,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub byte_count: usize,
    /// Mime type of the stored payload (e.g. `image/png`, `image/tiff`).
    /// Optional so older rows that never carried a mime still deserialise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// In-memory bytes carried from capture → factory → storage. Always
    /// `None` after deserialisation; the storage layer reads the same data
    /// out of `entries.payload_blob` instead.
    #[serde(skip)]
    pub pending_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileListContent {
    pub paths: Vec<String>,
    pub display_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichTextContent {
    pub plain_text: String,
    pub payload_ref: PayloadRef,
    /// Inline rich-text source (HTML or RTF) when the source pasteboard
    /// exposed it. Optional because the capture pipeline still falls back
    /// to plain text when `markup_kind` is unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markup_kind: Option<RichTextMarkup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RichTextMarkup {
    Html,
    Rtf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownContent {
    pub mime_type: Option<String>,
    pub payload_ref: Option<PayloadRef>,
    pub plain_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayloadRef {
    InlineText,
    DatabaseBlob(String),
    ContentAddressedFile { sha256: String, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMetadata {
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub use_count: u32,
    pub source: Option<SourceApp>,
    pub content_hash: ContentHash,
}

impl EntryMetadata {
    pub fn new(content_hash: ContentHash, source: Option<SourceApp>) -> Self {
        let now = OffsetDateTime::now_utc();
        Self {
            created_at: now,
            updated_at: now,
            last_used_at: None,
            use_count: 0,
            source,
            content_hash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceApp {
    pub bundle_id: Option<String>,
    pub name: Option<String>,
    pub executable_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHash {
    pub algorithm: HashAlgorithm,
    pub value: String,
}

impl ContentHash {
    pub fn sha256(content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(content);
        Self {
            algorithm: HashAlgorithm::Sha256,
            value: hex::encode(hasher.finalize()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HashAlgorithm {
    Sha256,
}

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
fn keyword_followed_by_whitespace(text: &str, keyword: &str) -> bool {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Sensitivity {
    #[default]
    Unknown,
    Public,
    Private,
    Secret,
    Blocked,
}

/// Whether the entry's plain text is safe to ship in default DTOs/outputs.
///
/// Only `Public` / `Unknown` text is admitted verbatim. `Private` and
/// `Secret` always drop to preview-only on the default path; `Blocked`
/// joins them defensively — the capture loop refuses to persist `Blocked`
/// rows today, but a stale row from an older daemon, a future import
/// path, or a corrupted DB could still surface here, so the helper fails
/// closed rather than trusting the upstream gate. Callers that want the
/// raw text regardless must opt in (e.g. `--include-sensitive` on the CLI
/// or the dedicated "show sensitive" UI affordance).
#[must_use]
pub const fn is_text_safe_for_default_output(sensitivity: Sensitivity) -> bool {
    matches!(sensitivity, Sensitivity::Public | Sensitivity::Unknown)
}

/// Marker substituted for the stored preview when an entry's preview text
/// cannot be trusted for default DTO/output paths.
///
/// `Private` and `Secret` rows already carry a redacted preview produced by
/// the classifier, so they pass through unchanged. `Blocked` rows do not —
/// the classifier never sets `redacted_preview` for them, and the daemon
/// refuses to persist new ones, so any `Blocked` row encountered here is
/// stale/imported and its `search.preview` is still raw text. Callers that
/// want the raw value regardless must opt in via `include_text` on the
/// caller side.
pub const BLOCKED_PREVIEW_PLACEHOLDER: &str = "[blocked]";

/// Pick the preview string to ship for `entry` on default DTO/output paths.
///
/// Returns the stored `entry.search.preview` for non-`Blocked` rows (where
/// the classifier has already replaced the preview with a redacted version
/// for `Private` / `Secret`). For `Blocked` rows the stored preview is
/// raw-derived, so substitute [`BLOCKED_PREVIEW_PLACEHOLDER`] to keep the
/// fail-closed contract that pairs with [`is_text_safe_for_default_output`].
#[must_use]
pub fn safe_preview_for_dto(entry: &ClipboardEntry) -> String {
    if matches!(entry.sensitivity, Sensitivity::Blocked) {
        BLOCKED_PREVIEW_PLACEHOLDER.to_owned()
    } else {
        entry.search.preview.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SensitivityReason {
    PasswordManagerSource,
    ApiKeyPattern,
    CreditCardPattern,
    PrivateKeyPattern,
    OneTimePasswordPattern,
    UserRegex,
    SourceAppDenylist,
    Oversized,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryLifecycle {
    pub pinned: bool,
    pub archived: bool,
    pub deleted_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardSnapshot {
    pub sequence: ClipboardSequence,
    pub captured_at: OffsetDateTime,
    pub source: Option<SourceApp>,
    pub representations: Vec<ClipboardRepresentation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardRepresentation {
    pub mime_type: String,
    pub data: ClipboardData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardData {
    Text(String),
    Bytes(Vec<u8>),
    FilePaths(Vec<String>),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ContentKind {
    Text,
    Url,
    Code,
    Image,
    FileList,
    RichText,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAction {
    pub id: AiActionId,
    pub name: String,
    pub input_policy: AiInputPolicy,
    pub output_policy: AiOutputPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum AiActionId {
    Summarize,
    Translate,
    FormatJson,
    FormatMarkdown,
    ExplainCode,
    Rewrite,
    ExtractTasks,
    RedactSecrets,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiInputPolicy {
    pub allow_remote: bool,
    pub require_redaction: bool,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiOutputPolicy {
    pub may_create_entry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiOutput {
    pub text: String,
    pub created_entry: Option<EntryId>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_content_counts_unicode_lines_and_bytes() {
        let content = TextContent::new("alpha\n日本".to_owned());

        assert_eq!(content.char_count, 8);
        assert_eq!(content.byte_count, 12);
        assert_eq!(content.line_count, 2);
    }

    #[test]
    fn plain_text_classifies_urls_and_normalizes_domain() {
        let content = ClipboardContent::from_plain_text(" https://Example.COM/path/ ");

        let ClipboardContent::Url(url) = content else {
            panic!("expected URL content");
        };
        assert_eq!(url.raw, " https://Example.COM/path/ ");
        assert_eq!(url.normalized, "https://example.com/path");
        assert_eq!(url.domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn plain_text_classifies_multiline_code() {
        let content = ClipboardContent::from_plain_text("fn main() {\n    println!(\"hi\");\n}");

        assert_eq!(content.kind(), ContentKind::Code);
        assert_eq!(
            content.plain_text(),
            Some("fn main() {\n    println!(\"hi\");\n}")
        );
    }

    #[test]
    fn plain_text_does_not_misclassify_keyword_as_code_substring() {
        // "fn" / "function" appearing inside identifiers must not trigger the
        // Code heuristic; word boundaries are required on both sides.
        let mixed = "trailing somefn matter\nsecond line of prose";
        let content = ClipboardContent::from_plain_text(mixed);
        assert_eq!(content.kind(), ContentKind::Text);

        let mixed2 = "the myfunction word\nanother prose line";
        let content2 = ClipboardContent::from_plain_text(mixed2);
        assert_eq!(content2.kind(), ContentKind::Text);

        // URL path segments like /function/ have non-word boundaries on both
        // sides but are not followed by whitespace, so they must not match.
        let url_segment = "see notes here\ndocs at /function/index and /class/foo too";
        let content3 = ClipboardContent::from_plain_text(url_segment);
        assert_eq!(content3.kind(), ContentKind::Text);

        // But a real keyword token still counts as code.
        let real = ClipboardContent::from_plain_text("intro line\nfn helper() {}\n");
        assert_eq!(real.kind(), ContentKind::Code);
    }

    #[test]
    fn search_document_builds_preview_title_and_tokens() {
        let id = EntryId::new();
        let content = ClipboardContent::from_plain_text("https://example.com/docs");
        let doc = SearchDocument::new(id, &content, "example docs clipboard".to_owned());

        assert_eq!(doc.entry_id, id);
        assert_eq!(doc.title.as_deref(), Some("example.com"));
        assert_eq!(doc.preview, "https://example.com/docs");
        assert_eq!(
            doc.tokens,
            vec![
                "example".to_owned(),
                "docs".to_owned(),
                "clipboard".to_owned()
            ]
        );
    }

    #[test]
    fn preview_compacts_whitespace_and_truncates_by_chars() {
        assert_eq!(make_preview("  one\n\n two\tthree  ", 100), "one two three");
        // Ellipsis counts toward max_chars: total length is exactly max_chars.
        assert_eq!(make_preview("日本語テキスト", 3), "日本…");
        assert_eq!(make_preview("日本語テキスト", 3).chars().count(), 3);
        // No truncation when text fits exactly.
        assert_eq!(make_preview("abc", 3), "abc");
        // max_chars == 0 cannot fit even an ellipsis, so return empty.
        assert_eq!(make_preview("abc", 0), "");
    }

    #[test]
    fn is_text_safe_for_default_output_only_admits_public_and_unknown() {
        assert!(is_text_safe_for_default_output(Sensitivity::Public));
        assert!(is_text_safe_for_default_output(Sensitivity::Unknown));
        assert!(!is_text_safe_for_default_output(Sensitivity::Blocked));
        assert!(!is_text_safe_for_default_output(Sensitivity::Private));
        assert!(!is_text_safe_for_default_output(Sensitivity::Secret));
    }

    #[test]
    fn safe_preview_for_dto_replaces_blocked_preview_only() {
        // Public/Unknown previews are raw-derived but the sensitivity is
        // safe, so they pass through. Private/Secret previews carry the
        // classifier's redacted_preview already, so they pass through too.
        // Blocked previews are still raw text (the classifier never sets
        // redacted_preview for them) and must be replaced.
        let mut entry = crate::EntryFactory::from_text("super secret value");
        entry.search.preview = "super secret value".to_owned();

        for safe in [Sensitivity::Public, Sensitivity::Unknown] {
            entry.sensitivity = safe;
            assert_eq!(safe_preview_for_dto(&entry), "super secret value");
        }
        for redacted in [Sensitivity::Private, Sensitivity::Secret] {
            entry.sensitivity = redacted;
            assert_eq!(safe_preview_for_dto(&entry), "super secret value");
        }
        entry.sensitivity = Sensitivity::Blocked;
        assert_eq!(safe_preview_for_dto(&entry), BLOCKED_PREVIEW_PLACEHOLDER);
    }
}

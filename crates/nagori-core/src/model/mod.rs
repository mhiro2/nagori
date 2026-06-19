use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::limits::ReadBudget;

mod ai;
pub mod code_language;
mod content;
mod file_path;
mod file_summary;
mod paste_option;
mod representations;
mod search;
mod semantic;

pub use ai::*;
pub use content::*;
pub use file_path::*;
pub use file_summary::*;
pub use paste_option::*;
pub use representations::*;
pub use search::*;
pub use semantic::*;

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
    /// Validated representations the storage layer should persist alongside
    /// the primary `content`. Populated by [`crate::EntryFactory`] from a
    /// `ClipboardSnapshot` and drained by the storage layer's insert path —
    /// `#[serde(skip)]` so the JSON envelope on disk never grows by the
    /// alternative payloads and the field is always empty after a round-trip
    /// through `EntryRepository::get`. Mirrors the lifetime contract that
    /// `ImageContent::pending_bytes` already uses for primary image bytes.
    #[serde(skip)]
    pub pending_representations: Vec<StoredClipboardRepresentation>,
}

impl ClipboardEntry {
    pub const fn content_kind(&self) -> ContentKind {
        self.content.kind()
    }

    pub fn plain_text(&self) -> Option<&str> {
        self.content.plain_text()
    }

    /// Trim `pending_representations` from the tail until each content kind's
    /// representations fit inside that kind's budget.
    ///
    /// Image-shaped representations (mime `image/*`, stored as a database blob)
    /// are measured against `budget.image_bytes`; everything else (plain /
    /// html / rtf / file-list) against `budget.text_bytes`. Splitting the two
    /// is what lets a multi-megabyte screenshot keep its primary image while
    /// any text alternatives still answer to the smaller text budget — and
    /// stops a large image alternative from being forced out under the text
    /// budget.
    ///
    /// The primary representation is never trimmed — callers gate "primary
    /// alone is oversized" upstream (see capture\_loop's `payload_bytes`
    /// check) and drop the whole entry instead. Returns whether any
    /// representation was removed; when something was trimmed the caller is
    /// responsible for recomputing `metadata.representation_set_hash`.
    pub fn trim_alternatives_to_budget(&mut self, budget: ReadBudget) -> bool {
        if self.pending_representations.is_empty() {
            return false;
        }
        let mut image_total: usize = 0;
        let mut text_total: usize = 0;
        for rep in &self.pending_representations {
            if is_image_representation(rep) {
                image_total = image_total.saturating_add(rep.byte_count());
            } else {
                text_total = text_total.saturating_add(rep.byte_count());
            }
        }
        if image_total <= budget.image_bytes && text_total <= budget.text_bytes {
            return false;
        }
        let original_len = self.pending_representations.len();
        // Drop the tail-most alternative belonging to a kind that is still over
        // its budget, leaving the primary and the other kind untouched.
        // Alternatives carry the largest ordinals, so trimming from the tail
        // preserves the role ordering the factory established. The `Primary`
        // role is skipped explicitly: dropping it is never correct here (an
        // oversized primary is rejected as a whole entry upstream).
        while self.pending_representations.len() > 1
            && (image_total > budget.image_bytes || text_total > budget.text_bytes)
        {
            let Some(idx) = self
                .pending_representations
                .iter()
                .enumerate()
                .rev()
                .find_map(|(idx, rep)| {
                    if rep.role == RepresentationRole::Primary {
                        return None;
                    }
                    let over_budget = if is_image_representation(rep) {
                        image_total > budget.image_bytes
                    } else {
                        text_total > budget.text_bytes
                    };
                    over_budget.then_some(idx)
                })
            else {
                // The only over-budget kind has nothing left to drop but the
                // primary — stop rather than spin.
                break;
            };
            let dropped = self.pending_representations.remove(idx);
            if is_image_representation(&dropped) {
                image_total = image_total.saturating_sub(dropped.byte_count());
            } else {
                text_total = text_total.saturating_sub(dropped.byte_count());
            }
            tracing::debug!(
                role = dropped.role.as_str(),
                mime_type = %dropped.mime_type,
                ordinal = dropped.ordinal,
                byte_count = dropped.byte_count(),
                "representation_dropped_for_budget"
            );
        }
        self.pending_representations.len() != original_len
    }
}

/// Whether a stored representation holds an image payload, and so answers to
/// the image byte budget rather than the text budget.
///
/// Keyed on the `image/*` mime prefix, with the database-blob payload variant
/// as a backstop — the two always agree today (image bytes are the only blobs)
/// but the explicit check keeps the classification robust if that changes.
fn is_image_representation(rep: &StoredClipboardRepresentation) -> bool {
    rep.mime_type.starts_with("image/")
        || matches!(rep.data, RepresentationDataRef::DatabaseBlob(_))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMetadata {
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub use_count: u32,
    pub source: Option<SourceApp>,
    pub content_hash: ContentHash,
    /// SHA-256 over the set of preserved representations that copy-back
    /// would re-publish. While the capture pipeline only carries a single
    /// `role = 'primary'` representation per entry this stays equal to
    /// `content_hash`; once the snapshot's alternative representations
    /// (HTML + plain, RTF + plain, image + file URL, …) start flowing to
    /// storage the value diverges so dedupe can choose between "same
    /// primary content" and "same representation set". `#[serde(default)]`
    /// keeps older serialised entries readable without a migration of the
    /// JSON payload.
    #[serde(default)]
    pub representation_set_hash: Option<ContentHash>,
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
            representation_set_hash: Some(content_hash.clone()),
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
    safe_preview_str(entry.sensitivity, &entry.search.preview)
}

/// The [`safe_preview_for_dto`] decision for callers that hold only a row's
/// projected `sensitivity` and stored `preview`, not the full entry.
///
/// A search-result projection is the motivating case. Substitutes
/// [`BLOCKED_PREVIEW_PLACEHOLDER`] for `Blocked` rows (whose stored preview is
/// still raw text) and passes every other sensitivity through, since the
/// classifier has already redacted the `Private` / `Secret` previews at capture
/// time.
#[must_use]
pub fn safe_preview_str(sensitivity: Sensitivity, preview: &str) -> String {
    if matches!(sensitivity, Sensitivity::Blocked) {
        BLOCKED_PREVIEW_PLACEHOLDER.to_owned()
    } else {
        preview.to_owned()
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
    fn uppercase_scheme_still_classifies_as_url() {
        // The WHATWG parser lower-cases the scheme, so an uppercase `HTTPS://`
        // is a real URL — it must classify as Url (with a normalized form) and
        // not fall through to Text, which would skip dedupe and normalization.
        let content = ClipboardContent::from_plain_text("HTTPS://Example.COM/Path");
        let ClipboardContent::Url(url) = content else {
            panic!("expected URL content for uppercase scheme");
        };
        assert_eq!(url.normalized, "https://example.com/Path");
        assert_eq!(url.domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn url_normalization_preserves_case_sensitive_path_and_query() {
        // Path / query / fragment are case-sensitive per RFC 3986 §6.2.2.1:
        // only the scheme and host may be lower-cased. Forcing the whole URL
        // to lowercase used to break S3 signed URLs (signature in query),
        // GitHub blob hashes, and CamelCase paths — none of which should
        // dedupe against their lowercased twin.
        let content = ClipboardContent::from_plain_text(
            "https://Example.COM/CasePath/File.PDF?Sig=AbCdEf#Section",
        );
        let ClipboardContent::Url(url) = content else {
            panic!("expected URL content");
        };
        assert_eq!(
            url.normalized,
            "https://example.com/CasePath/File.PDF?Sig=AbCdEf#Section"
        );
    }

    #[test]
    fn url_normalization_keeps_trailing_slash_in_query_and_fragment() {
        // A trailing `/` inside the query (e.g. a base64-ish signature) or the
        // fragment is data, not path separator noise — trimming it would
        // corrupt signed URLs and dedupe genuinely different ones.
        let cases = [
            "https://example.com/download?sig=AbC/",
            "https://example.com/path/?a=1",
            "https://example.com/docs#section/",
        ];
        for case in cases {
            let ClipboardContent::Url(url) = ClipboardContent::from_plain_text(case) else {
                panic!("expected URL content for {case}");
            };
            assert_eq!(url.normalized, case, "must keep {case} verbatim");
        }
    }

    #[test]
    fn url_normalization_trims_trailing_slash_without_query_or_fragment() {
        for (input, expected) in [
            ("https://example.com/", "https://example.com"),
            ("https://example.com/path/", "https://example.com/path"),
        ] {
            let ClipboardContent::Url(url) = ClipboardContent::from_plain_text(input) else {
                panic!("expected URL content for {input}");
            };
            assert_eq!(url.normalized, expected);
        }
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
    fn plain_text_sets_language_hint_for_code() {
        // Code-kind clips now carry a canonical language id so the preview
        // pane, result-row badge, and ranker all read the same value.
        let ClipboardContent::Code(code) =
            ClipboardContent::from_plain_text("fn main() {\n    println!(\"hi\");\n}")
        else {
            panic!("expected Code content");
        };
        assert_eq!(code.language_hint.as_deref(), Some("rust"));
    }

    #[test]
    fn plain_text_classifies_minified_json_as_code() {
        // Single-line minified JSON has no newline, so it used to fall through
        // to plain Text. It now classifies as Code with a `json` hint so the
        // JSON badge / highlight light up even when the body isn't pretty.
        let content = ClipboardContent::from_plain_text("{\"name\":\"nagori\",\"n\":1}");
        assert_eq!(content.kind(), ContentKind::Code);
        let ClipboardContent::Code(code) = content else {
            panic!("expected Code content");
        };
        assert_eq!(code.language_hint.as_deref(), Some("json"));
    }

    #[test]
    fn plain_text_keeps_brace_prose_as_text() {
        // Bracketed bodies that do not *parse* as JSON must stay Text rather
        // than being mislabelled as code: a brace-wrapped note, an array of
        // bare words, and broken JSON.
        assert_eq!(
            ClipboardContent::from_plain_text("{just a note}").kind(),
            ContentKind::Text
        );
        assert_eq!(
            ClipboardContent::from_plain_text("[foo, bar]").kind(),
            ContentKind::Text
        );
        assert_eq!(
            ClipboardContent::from_plain_text("{\"a\":}").kind(),
            ContentKind::Text
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
    fn representation_role_str_matches_db_values() {
        // `entry_representations.role` is a SQL string column; the
        // factory and storage layer agree on these literal values, so a
        // typo in either side would silently scramble role queries.
        assert_eq!(RepresentationRole::Primary.as_str(), "primary");
        assert_eq!(RepresentationRole::PlainFallback.as_str(), "plain_fallback");
        assert_eq!(RepresentationRole::Alternative.as_str(), "alternative");
    }

    #[test]
    fn stored_representation_byte_count_matches_persisted_shape() {
        let text = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("hello".to_owned()),
        };
        assert_eq!(text.byte_count(), 5);

        let blob = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "image/png".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::DatabaseBlob(vec![0, 1, 2, 3]),
        };
        assert_eq!(blob.byte_count(), 4);

        // JSON-array storage form: `["one","two"]` is 13 bytes.
        let paths = StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 2,
            data: RepresentationDataRef::FilePaths(vec!["one".to_owned(), "two".to_owned()]),
        };
        assert_eq!(paths.byte_count(), 13);
        assert_eq!(
            paths.byte_count(),
            encode_file_paths(&["one".to_owned(), "two".to_owned()]).len()
        );
    }

    #[test]
    fn file_paths_round_trip_preserves_embedded_newlines() {
        // A path containing a newline is legal on Unix and broke the old
        // newline-joined encoding (it split into two bogus entries). The JSON
        // encoding must round-trip it byte-for-byte.
        let paths = vec![
            "/tmp/normal.txt".to_owned(),
            "/tmp/weird\nname.txt".to_owned(),
        ];
        let encoded = encode_file_paths(&paths);
        assert_eq!(decode_file_paths(&encoded), paths);
    }

    #[test]
    fn decode_file_paths_falls_back_to_legacy_newline_split() {
        // Rows written by older builds are newline-joined and are not valid
        // JSON, so decoding must fall back to the legacy split.
        assert_eq!(
            decode_file_paths("/tmp/a.txt\n/tmp/b.txt"),
            vec!["/tmp/a.txt".to_owned(), "/tmp/b.txt".to_owned()],
        );
    }

    #[test]
    fn trim_alternatives_drops_tail_until_budget_fits() {
        let mut entry = crate::EntryFactory::from_text("primary");
        entry.pending_representations = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/plain".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::InlineText("primary".to_owned()),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "text/html".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::InlineText("a".repeat(100)),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "text/rtf".to_owned(),
                ordinal: 2,
                data: RepresentationDataRef::InlineText("b".repeat(50)),
            },
        ];
        // 7 + 100 + 50 = 157 text bytes. Text budget 60 → drop tail (50) →
        // still 107 → drop next (100) → 7 ≤ 60, stop. Primary always survives.
        let changed = entry.trim_alternatives_to_budget(ReadBudget::new(60, 60));
        assert!(changed);
        assert_eq!(entry.pending_representations.len(), 1);
        assert_eq!(
            entry.pending_representations[0].role,
            RepresentationRole::Primary
        );
    }

    #[test]
    fn trim_alternatives_is_noop_when_budget_fits() {
        let mut entry = crate::EntryFactory::from_text("primary");
        entry.pending_representations = vec![StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("primary".to_owned()),
        }];
        assert!(!entry.trim_alternatives_to_budget(ReadBudget::new(1_000_000, 1_000_000)));
        assert_eq!(entry.pending_representations.len(), 1);
    }

    #[test]
    fn trim_alternatives_never_drops_primary_even_if_oversized() {
        // A primary larger than the budget shouldn't be removed here —
        // the capture loop drops the whole entry upstream, but this
        // helper must keep primary so a misuse can't silently lose it.
        let mut entry = crate::EntryFactory::from_text("primary");
        entry.pending_representations = vec![StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("a".repeat(1000)),
        }];
        let _ = entry.trim_alternatives_to_budget(ReadBudget::new(10, 10));
        assert_eq!(entry.pending_representations.len(), 1);
    }

    #[test]
    fn trim_keeps_image_primary_and_trims_oversized_text_alternative() {
        // A screenshot with a bulky text alternative: the image primary fits
        // the (large) image budget, but the text alternative blows the (small)
        // text budget — only the text alternative should be dropped.
        let mut entry = crate::EntryFactory::from_text("primary");
        entry.pending_representations = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "image/png".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::DatabaseBlob(vec![0u8; 2000]),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "text/html".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::InlineText("x".repeat(2000)),
            },
        ];
        // image_total = 2000 ≤ 10_000; text_total = 2000 > 100 → drop the text alt.
        let changed = entry.trim_alternatives_to_budget(ReadBudget::new(100, 10_000));
        assert!(changed);
        assert_eq!(entry.pending_representations.len(), 1);
        assert_eq!(entry.pending_representations[0].mime_type, "image/png");
    }

    #[test]
    fn trim_keeps_text_primary_and_trims_oversized_image_alternative() {
        // The mirror case: a text primary within the text budget keeps an
        // over-the-image-budget image alternative from forcing the text out.
        let mut entry = crate::EntryFactory::from_text("primary");
        entry.pending_representations = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/plain".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::InlineText("hello".to_owned()),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::Alternative,
                mime_type: "image/png".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::DatabaseBlob(vec![0u8; 5000]),
            },
        ];
        // text_total = 5 ≤ 10_000; image_total = 5000 > 100 → drop the image alt.
        let changed = entry.trim_alternatives_to_budget(ReadBudget::new(10_000, 100));
        assert!(changed);
        assert_eq!(entry.pending_representations.len(), 1);
        assert_eq!(entry.pending_representations[0].mime_type, "text/plain");
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

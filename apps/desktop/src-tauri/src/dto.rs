use std::collections::BTreeMap;

use nagori_core::settings::{AiProviderSetting, OnboardingSettings};
use nagori_core::{
    AiOutput, AppSettings, Appearance, ClipboardContent, ClipboardEntry, ContentKind, EntryId,
    Locale, PaletteHotkeyAction, PasteFormat, RankReason, RecentOrder, RepresentationRole,
    RepresentationSummary, SearchFilters, SearchMode, SearchResult, SecondaryHotkeyAction,
    SecretHandling, Sensitivity, UpdateChannel, is_text_safe_for_default_output, normalize_text,
    safe_preview_for_dto,
};
use nagori_platform::{
    Capability, PermissionKind, PermissionState, PermissionStatus, Platform, PlatformCapabilities,
    SupportTier,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentKindDto {
    Text,
    Url,
    Code,
    Image,
    FileList,
    RichText,
    Unknown,
}

impl From<ContentKind> for ContentKindDto {
    fn from(kind: ContentKind) -> Self {
        match kind {
            ContentKind::Text => Self::Text,
            ContentKind::Url => Self::Url,
            ContentKind::Code => Self::Code,
            ContentKind::Image => Self::Image,
            ContentKind::FileList => Self::FileList,
            ContentKind::RichText => Self::RichText,
            ContentKind::Unknown => Self::Unknown,
        }
    }
}

impl From<ContentKindDto> for ContentKind {
    fn from(kind: ContentKindDto) -> Self {
        match kind {
            ContentKindDto::Text => Self::Text,
            ContentKindDto::Url => Self::Url,
            ContentKindDto::Code => Self::Code,
            ContentKindDto::Image => Self::Image,
            ContentKindDto::FileList => Self::FileList,
            ContentKindDto::RichText => Self::RichText,
            ContentKindDto::Unknown => Self::Unknown,
        }
    }
}

fn default_capture_kind_dtos() -> Vec<ContentKindDto> {
    nagori_core::settings::default_capture_kinds()
        .into_iter()
        .map(Into::into)
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RepresentationRoleDto {
    Primary,
    PlainFallback,
    Alternative,
}

impl From<RepresentationRole> for RepresentationRoleDto {
    fn from(role: RepresentationRole) -> Self {
        match role {
            RepresentationRole::Primary => Self::Primary,
            RepresentationRole::PlainFallback => Self::PlainFallback,
            RepresentationRole::Alternative => Self::Alternative,
        }
    }
}

/// Wire-safe projection of one preserved representation row. Mirrors
/// `nagori_ipc::RepresentationSummaryDto` but serialises in camelCase so
/// the Svelte side can consume the field without a transformation layer.
/// Bytes/text stay daemon-side; only the MIME type, role, and byte count
/// reach the renderer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentationSummaryDto {
    pub mime_type: String,
    pub role: RepresentationRoleDto,
    pub byte_count: u64,
}

impl RepresentationSummaryDto {
    pub fn from_summary(summary: &RepresentationSummary) -> Self {
        Self {
            mime_type: summary.mime_type.clone(),
            role: summary.role.into(),
            byte_count: summary.byte_count,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub preview: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<OffsetDateTime>,
    pub use_count: u32,
    pub pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    pub sensitivity: Sensitivity,
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl EntryDto {
    pub fn from_entry(entry: ClipboardEntry, include_text: bool) -> Self {
        let preview = safe_preview_for_dto(&entry);
        Self {
            id: entry.id,
            kind: entry.content_kind().into(),
            text: include_text.then(|| entry.plain_text().unwrap_or_default().to_owned()),
            preview,
            created_at: entry.metadata.created_at,
            updated_at: entry.metadata.updated_at,
            last_used_at: entry.metadata.last_used_at,
            use_count: entry.metadata.use_count,
            pinned: entry.lifecycle.pinned,
            source_app_name: entry.metadata.source.and_then(|source| source.name),
            sensitivity: entry.sensitivity,
            representation_summary: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_representation_summaries(mut self, summaries: &[RepresentationSummary]) -> Self {
        self.representation_summary = summaries
            .iter()
            .map(RepresentationSummaryDto::from_summary)
            .collect();
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    pub preview: String,
    pub score: f32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub pinned: bool,
    pub sensitivity: Sensitivity,
    pub rank_reasons: Vec<RankReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl From<SearchResult> for SearchResultDto {
    fn from(value: SearchResult) -> Self {
        Self {
            id: value.entry_id,
            kind: value.content_kind.into(),
            preview: value.preview,
            score: value.score,
            created_at: value.created_at,
            pinned: value.pinned,
            sensitivity: value.sensitivity,
            rank_reasons: value.rank_reason,
            source_app_name: value.source_app_name,
            representation_summary: Vec::new(),
        }
    }
}

impl SearchResultDto {
    #[must_use]
    pub fn with_representation_summaries(mut self, summaries: &[RepresentationSummary]) -> Self {
        self.representation_summary = summaries
            .iter()
            .map(RepresentationSummaryDto::from_summary)
            .collect();
        self
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFiltersDto {
    #[serde(default)]
    pub kinds: Vec<ContentKindDto>,
    #[serde(default)]
    pub pinned_only: bool,
    #[serde(default)]
    pub source_app: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_after: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_before: Option<OffsetDateTime>,
}

impl From<SearchFiltersDto> for SearchFilters {
    fn from(value: SearchFiltersDto) -> Self {
        Self {
            kinds: value.kinds.into_iter().map(Into::into).collect(),
            pinned_only: value.pinned_only,
            source_app: value.source_app,
            created_after: value.created_after,
            created_before: value.created_before,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequestDto {
    pub query: String,
    #[serde(default)]
    pub mode: Option<SearchMode>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub filters: Option<SearchFiltersDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponseDto {
    pub results: Vec<SearchResultDto>,
    pub total_candidates: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPreviewDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    pub title: Option<String>,
    pub preview_text: String,
    pub body: PreviewBodyDto,
    pub metadata: EntryPreviewMetadataDto,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPreviewMetadataDto {
    pub byte_count: usize,
    pub char_count: usize,
    pub line_count: usize,
    // Kept for forward-compat with non-bundled callers (CLI, IPC). The
    // frontend dispatches on `truncation` instead.
    pub truncated: bool,
    pub truncation: TruncationDto,
    pub sensitive: bool,
    pub full_content_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    // best-effort signal: true when the user's current search query matches
    // text inside the elided middle (so the renderer can warn that a hit
    // is hidden). `None` when no query was passed or the body wasn't
    // truncated. Not synced with FTS — substring-match only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elided_contains_match: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TruncationDto {
    None,
    HeadOnly,
    HeadAndTail { elided_bytes: usize },
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PreviewBodyDto {
    Text {
        text: String,
    },
    Code {
        text: String,
        language: Option<String>,
    },
    Url {
        url: String,
        domain: Option<String>,
        // Structured decomposition of `url` so the renderer can show
        // `scheme://host` and `/path?query` on separate visual rows. All
        // three new fields are `None` (or `null` after camelCase JSON
        // rendering) when `url::Url::parse` rejected the body — the UI
        // falls back to the flat `url` string in that case.
        #[serde(skip_serializing_if = "Option::is_none")]
        scheme: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        host_display: Option<String>,
        // Only emitted when the IDN punycode form differs from
        // `host_display` (i.e. the user-facing host is non-ASCII Unicode).
        // The renderer surfaces a phishing-resistance badge when this is
        // `Some`. Stays `None` for plain ASCII domains.
        #[serde(skip_serializing_if = "Option::is_none")]
        host_punycode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path_and_query: Option<String>,
    },
    Image {
        mime_type: Option<String>,
        byte_count: usize,
        width: Option<u32>,
        height: Option<u32>,
    },
    FileList {
        paths: Vec<String>,
        // Pre-truncation `paths.len()`. The wire `paths` is capped at 50
        // entries so the renderer can show `paths.length / total` without
        // re-counting and surface a "+N more" hint when the underlying
        // clipboard list is longer.
        total: usize,
    },
    RichText {
        text: String,
    },
    Unknown {
        text: String,
    },
}

/// Default soft cap on preview byte length. Head+tail truncation kicks in
/// above this threshold so the user keeps the tail context (closing
/// braces, footer signatures) that a head-only cut would lose.
pub const MAX_PREVIEW_BYTES: usize = 128 * 1024;
/// Line-based cap applied before the byte cap. Keeps highlight tokenisation
/// and the gutter renderer bounded even for files whose lines are short
/// enough to stay under `MAX_PREVIEW_BYTES` (e.g. a 50k-line log).
pub const MAX_PREVIEW_LINES: usize = 4_000;
/// Byte cap for `get_entry_preview_full`. Higher than the default cap but
/// still bounded — the full preview pane is opt-in (expanded mode) and
/// hands the entire window over to the body, so an unbounded payload
/// would block the renderer on multi-MB clips.
pub const MAX_PREVIEW_FULL_BYTES: usize = 1024 * 1024;

impl EntryPreviewDto {
    // Default constructor used by tests and as the documented entry point;
    // production code paths supply an optional query via
    // `from_entry_with_query` so the elided-match hint can flow through.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_entry(entry: &ClipboardEntry) -> Self {
        Self::build(entry, MAX_PREVIEW_BYTES, None)
    }

    /// Same as `from_entry` but tags `elided_contains_match` when the
    /// supplied search query (raw user input — not normalised) appears in
    /// the middle region we just elided. Empty queries are treated as
    /// "no query" so the renderer never emits a misleading warning on a
    /// pristine preview pane.
    pub fn from_entry_with_query(entry: &ClipboardEntry, query: Option<&str>) -> Self {
        let trimmed = query.map(str::trim).filter(|q| !q.is_empty());
        Self::build(entry, MAX_PREVIEW_BYTES, trimmed)
    }

    /// Build a preview with a larger byte cap (used by `get_entry_preview_full`).
    /// Sensitive entries are still redacted to the safe-preview placeholder
    /// at the caller; this method does not relax sensitivity gating.
    pub fn from_entry_full(entry: &ClipboardEntry) -> Self {
        Self::build(entry, MAX_PREVIEW_FULL_BYTES, None)
    }

    fn build(entry: &ClipboardEntry, byte_cap: usize, query: Option<&str>) -> Self {
        let sensitive = !is_text_safe_for_default_output(entry.sensitivity);
        let raw_text = if sensitive {
            safe_preview_for_dto(entry)
        } else {
            entry.plain_text().unwrap_or_default().to_owned()
        };
        let truncation_result = truncate_for_preview(&raw_text, byte_cap, MAX_PREVIEW_LINES);
        let preview_text = truncation_result.text;
        let truncation_dto = truncation_result.truncation;
        let truncated = !matches!(truncation_dto, TruncationDto::None);
        // Apply the same normalizer used by the FTS pipeline (NFKC + lowercase
        // + whitespace collapse) so the hint matches whether the *search*
        // would have hit text inside the elided middle. Per-term `all()` keeps
        // a multi-token query like "foo bar" honest when only some of its
        // tokens fall in the elided window.
        let elided_contains_match = match (query, truncation_result.elided_region) {
            (Some(q), Some((start, end))) if start < end && end <= raw_text.len() => {
                let normalized_q = normalize_text(q);
                if normalized_q.is_empty() {
                    None
                } else {
                    let normalized_region = normalize_text(&raw_text[start..end]);
                    let hit = normalized_q
                        .split_whitespace()
                        .all(|term| normalized_region.contains(term));
                    Some(hit)
                }
            }
            _ => None,
        };
        // Mirror the IPC gate on `get_entry_preview_full` (Public-only). If the
        // entry isn't Public, the expand button would just trigger a forbidden
        // response, so we hide it at the source.
        let full_content_available = matches!(entry.sensitivity, Sensitivity::Public);
        let title = entry.search.title.clone();
        let language = entry.search.language.clone();
        let domain = match &entry.content {
            ClipboardContent::Url(value) => value.domain.clone(),
            _ => None,
        };
        let body = if sensitive {
            PreviewBodyDto::Text {
                text: preview_text.clone(),
            }
        } else {
            match &entry.content {
                ClipboardContent::Text(_) => PreviewBodyDto::Text {
                    text: preview_text.clone(),
                },
                ClipboardContent::Code(value) => PreviewBodyDto::Code {
                    text: preview_text.clone(),
                    language: value.language_hint.clone().or_else(|| language.clone()),
                },
                ClipboardContent::Url(value) => {
                    let parts = UrlParts::from_raw(&value.raw);
                    PreviewBodyDto::Url {
                        url: preview_text.clone(),
                        domain: value.domain.clone(),
                        scheme: parts.as_ref().map(|p| p.scheme.clone()),
                        host_display: parts.as_ref().map(|p| p.host_display.clone()),
                        host_punycode: parts.as_ref().and_then(|p| p.host_punycode.clone()),
                        path_and_query: parts.as_ref().map(|p| p.path_and_query.clone()),
                    }
                }
                ClipboardContent::Image(value) => PreviewBodyDto::Image {
                    mime_type: value.mime_type.clone(),
                    byte_count: value.byte_count,
                    width: value.width,
                    height: value.height,
                },
                ClipboardContent::FileList(value) => PreviewBodyDto::FileList {
                    paths: value.paths.iter().take(50).cloned().collect(),
                    total: value.paths.len(),
                },
                ClipboardContent::RichText(_) => PreviewBodyDto::RichText {
                    text: preview_text.clone(),
                },
                ClipboardContent::Unknown(_) => PreviewBodyDto::Unknown {
                    text: preview_text.clone(),
                },
            }
        };
        Self {
            id: entry.id,
            kind: entry.content_kind().into(),
            title,
            preview_text,
            body,
            metadata: EntryPreviewMetadataDto {
                byte_count: raw_text.len(),
                char_count: raw_text.chars().count(),
                line_count: raw_text.lines().count().max(1),
                truncated,
                truncation: truncation_dto,
                sensitive,
                full_content_available,
                domain,
                language,
                elided_contains_match,
            },
        }
    }
}

/// Three-way decomposition of a URL body used by the preview pane to
/// render `host`, `scheme://path`, and an optional punycode badge on
/// separate rows. Built from the entry's raw URL via `url::Url::parse`
/// and `idna::domain_to_unicode`, so we never trust the user-supplied
/// string for the display split.
#[derive(Debug, Clone)]
pub(crate) struct UrlParts {
    pub scheme: String,
    pub host_display: String,
    /// `Some` only when the ASCII (punycode) host differs from
    /// `host_display`. Plain ASCII domains leave this `None` so the
    /// renderer can skip the badge.
    pub host_punycode: Option<String>,
    pub path_and_query: String,
}

impl UrlParts {
    /// Best-effort parse. Returns `None` when the body isn't a syntactically
    /// valid absolute URL (e.g. a `mailto:` without an addr-spec) or the
    /// host part is empty — the caller falls back to a flat URL render.
    pub fn from_raw(raw: &str) -> Option<Self> {
        let parsed = url::Url::parse(raw.trim()).ok()?;
        let scheme = parsed.scheme().to_owned();
        let host_ascii = parsed.host_str()?.to_owned();
        if host_ascii.is_empty() {
            return None;
        }
        // `host_str()` returns the IDNA-A form (`xn--…`). Convert back to
        // Unicode for the display row; on conversion error fall back to
        // the ASCII string so the renderer still has something to show.
        // Non-default ports are folded into `host_display` (and the
        // punycode badge value when surfaced) so the confirm modal cannot
        // hide a redirect to `:8443` behind a familiar-looking hostname.
        let (unicode, errors) = idna::domain_to_unicode(&host_ascii);
        let port_suffix = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
        let (host_display, host_punycode) = if errors.is_ok() && unicode != host_ascii {
            (
                format!("{unicode}{port_suffix}"),
                Some(format!("{host_ascii}{port_suffix}")),
            )
        } else {
            (format!("{host_ascii}{port_suffix}"), None)
        };
        let mut path_and_query = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            path_and_query.push('?');
            path_and_query.push_str(query);
        }
        if let Some(fragment) = parsed.fragment() {
            path_and_query.push('#');
            path_and_query.push_str(fragment);
        }
        if path_and_query.is_empty() {
            path_and_query.push('/');
        }
        Some(Self {
            scheme,
            host_display,
            host_punycode,
            path_and_query,
        })
    }
}

#[derive(Debug, Clone)]
struct TruncationResult {
    text: String,
    truncation: TruncationDto,
    // Byte offsets in the original `raw_text` that were elided. `None`
    // when nothing was dropped. Used to spot-check whether a search hit
    // landed in the hidden middle.
    elided_region: Option<(usize, usize)>,
}

/// Cap a preview body by line count first, then by byte length. Returns a
/// head + sentinel + tail string with a single elided region in the
/// middle. Falls back to a head-only cut when the body is small enough
/// that head+tail would round-trip the entire string anyway.
fn truncate_for_preview(value: &str, max_bytes: usize, max_lines: usize) -> TruncationResult {
    if value.len() <= max_bytes && line_count(value) <= max_lines {
        return TruncationResult {
            text: value.to_owned(),
            truncation: TruncationDto::None,
            elided_region: None,
        };
    }
    if let Some(line_trimmed) = head_tail_truncate_lines(value, max_lines) {
        // Lines fell first. If the joined head+tail still busts the byte cap
        // (long lines), fall through to byte-cap truncation on the
        // original `value` so we don't double-elide. Otherwise emit the
        // line-trimmed string.
        if line_trimmed.text.len() <= max_bytes {
            return line_trimmed;
        }
    }
    head_tail_truncate_utf8(value, max_bytes)
}

fn line_count(value: &str) -> usize {
    // Match the existing metadata path: empty → 1, otherwise count newlines
    // and add one if the body doesn't end with `\n`.
    if value.is_empty() {
        return 1;
    }
    let trailing_nl = value.ends_with('\n');
    let nl = value.matches('\n').count();
    if trailing_nl { nl.max(1) } else { nl + 1 }
}

fn head_tail_truncate_lines(value: &str, max_lines: usize) -> Option<TruncationResult> {
    if line_count(value) <= max_lines || max_lines < 2 {
        return None;
    }
    // Half each side; bias the head slightly when `max_lines` is odd.
    let tail_lines = max_lines / 2;
    let head_lines = max_lines - tail_lines;
    let lines: Vec<&str> = value.lines().collect();
    if lines.len() <= max_lines {
        return None;
    }
    // Reconstruct head / tail by absolute byte offsets so the elided range
    // lines up with the original `value`.
    let head_end = byte_offset_after_lines(value, head_lines);
    let tail_start = byte_offset_before_last_lines(value, tail_lines);
    if head_end >= tail_start {
        return None;
    }
    let head = &value[..head_end];
    let tail = &value[tail_start..];
    let elided_bytes = tail_start - head_end;
    let elided_lines = lines.len() - head_lines - tail_lines;
    let sentinel = format!("\n… {elided_lines} lines elided ({elided_bytes} bytes) …\n");
    let mut out = String::with_capacity(head.len() + sentinel.len() + tail.len());
    out.push_str(head);
    out.push_str(&sentinel);
    out.push_str(tail);
    Some(TruncationResult {
        text: out,
        truncation: TruncationDto::HeadAndTail { elided_bytes },
        elided_region: Some((head_end, tail_start)),
    })
}

fn byte_offset_after_lines(value: &str, lines: usize) -> usize {
    if lines == 0 {
        return 0;
    }
    let mut count = 0_usize;
    for (idx, _) in value.match_indices('\n') {
        count += 1;
        if count == lines {
            return idx + 1; // include the trailing newline
        }
    }
    value.len()
}

fn byte_offset_before_last_lines(value: &str, lines: usize) -> usize {
    if lines == 0 {
        return value.len();
    }
    // Walk backwards through the newlines, ignoring the trailing newline
    // (if any) so the final visible line counts as line 1 of the tail.
    let bytes = value.as_bytes();
    let mut i = value.len();
    if i > 0 && bytes[i - 1] == b'\n' {
        i -= 1;
    }
    let mut found = 0_usize;
    while i > 0 {
        i -= 1;
        if bytes[i] == b'\n' {
            found += 1;
            if found == lines {
                return i + 1;
            }
        }
    }
    0
}

fn head_tail_truncate_utf8(value: &str, max_bytes: usize) -> TruncationResult {
    if value.len() <= max_bytes {
        return TruncationResult {
            text: value.to_owned(),
            truncation: TruncationDto::None,
            elided_region: None,
        };
    }
    // Reserve room for the sentinel so the final string honours the byte
    // budget. If the sentinel itself doesn't fit, fall back to head-only.
    let half = max_bytes / 2;
    // Probe sentinel length using the worst-case elided count.
    let elided_bytes_estimate = value.len() - max_bytes;
    let sentinel_probe = format!("\n… {elided_bytes_estimate} bytes elided …\n");
    if sentinel_probe.len() + 16 > max_bytes {
        // Tiny cap: degrade to head-only ellipsis to keep the rendered
        // body coherent. Truncation is still flagged so the UI can warn.
        let mut end = max_bytes.saturating_sub('…'.len_utf8());
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = value[..end].to_owned();
        out.push('…');
        return TruncationResult {
            text: out,
            truncation: TruncationDto::HeadOnly,
            elided_region: Some((end, value.len())),
        };
    }
    let mut head_end = half.saturating_sub(sentinel_probe.len() / 2);
    while head_end > 0 && !value.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = value
        .len()
        .saturating_sub(max_bytes - head_end - sentinel_probe.len());
    while tail_start < value.len() && !value.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    if tail_start <= head_end {
        // Defensive: pathological inputs (tiny `max_bytes`) — fall back.
        let mut end = max_bytes.saturating_sub('…'.len_utf8());
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = value[..end].to_owned();
        out.push('…');
        return TruncationResult {
            text: out,
            truncation: TruncationDto::HeadOnly,
            elided_region: Some((end, value.len())),
        };
    }
    let elided_bytes = tail_start - head_end;
    let sentinel = format!("\n… {elided_bytes} bytes elided …\n");
    let mut out = String::with_capacity(head_end + sentinel.len() + (value.len() - tail_start));
    out.push_str(&value[..head_end]);
    out.push_str(&sentinel);
    out.push_str(&value[tail_start..]);
    TruncationResult {
        text: out,
        truncation: TruncationDto::HeadAndTail { elided_bytes },
        elided_region: Some((head_end, tail_start)),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiActionResultDto {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_entry_id: Option<EntryId>,
    pub warnings: Vec<String>,
}

impl From<AiOutput> for AiActionResultDto {
    fn from(value: AiOutput) -> Self {
        Self {
            text: value.text,
            created_entry_id: value.created_entry,
            warnings: value.warnings,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionKindDto {
    Accessibility,
    InputMonitoring,
    Clipboard,
    Notifications,
    AutoLaunch,
}

impl From<PermissionKind> for PermissionKindDto {
    fn from(value: PermissionKind) -> Self {
        match value {
            PermissionKind::Accessibility => Self::Accessibility,
            PermissionKind::InputMonitoring => Self::InputMonitoring,
            PermissionKind::Clipboard => Self::Clipboard,
            PermissionKind::Notifications => Self::Notifications,
            PermissionKind::AutoLaunch => Self::AutoLaunch,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStateDto {
    Granted,
    Denied,
    NotDetermined,
    Unsupported,
}

impl From<PermissionState> for PermissionStateDto {
    fn from(value: PermissionState) -> Self {
        match value {
            PermissionState::Granted => Self::Granted,
            PermissionState::Denied => Self::Denied,
            PermissionState::NotDetermined => Self::NotDetermined,
            PermissionState::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatusDto {
    pub kind: PermissionKindDto,
    pub state: PermissionStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Stable identifier (e.g. `"accessibility_not_prompted"`) so the
    /// frontend can branch without scraping the message string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Deep-link target inside the Settings window (e.g.
    /// `"setup/accessibility"`) used by the `StatusBar` indicator click
    /// handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_route: Option<String>,
    /// Permalink to the relevant docs section, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
}

/// Wire-shape mirror of `state::HotkeyFailureRecord`. Returned by the
/// `last_hotkey_failure` command so the always-on App-level subscriber
/// can re-hydrate the toast/banner if the live event fired before its
/// listener attached. The field shape matches the
/// `nagori://hotkey_register_failed` emit envelope so the frontend
/// store can share a single normaliser between the two paths.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyFailureDto {
    pub hotkey: String,
    pub error: String,
    /// `Some("secondary")` for secondary accelerators; absent for the
    /// primary palette shortcut — mirrors `build_hotkey_failure_payload`
    /// in `lib.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Kebab-case wire value of the secondary action whose register
    /// failed (`repaste-last`, `clear-history`). Absent for primary
    /// failures. The frontend store reads this so a later resolved
    /// event targeting a *different* secondary action can be ignored
    /// instead of wiping the displayed banner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

impl From<crate::state::HotkeyFailureRecord> for HotkeyFailureDto {
    fn from(value: crate::state::HotkeyFailureRecord) -> Self {
        Self {
            hotkey: value.hotkey,
            error: value.error,
            kind: value.kind,
            action: value.action,
        }
    }
}

impl From<PermissionStatus> for PermissionStatusDto {
    fn from(value: PermissionStatus) -> Self {
        Self {
            kind: value.kind.into(),
            state: value.state.into(),
            message: value.message,
            reason_code: value.reason_code,
            setup_route: value.setup_route,
            docs_url: value.docs_url,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlatformDto {
    // Match the IPC JSON shape (`"macos"`) rather than the camelCase
    // derive's `"macOs"` so the frontend can treat the platform name as
    // a stable identifier across CLI / IPC / Tauri surfaces.
    #[serde(rename = "macos")]
    MacOS,
    Windows,
    LinuxWayland,
    Unsupported,
}

impl From<Platform> for PlatformDto {
    fn from(value: Platform) -> Self {
        match value {
            Platform::MacOS => Self::MacOS,
            Platform::Windows => Self::Windows,
            Platform::LinuxWayland => Self::LinuxWayland,
            Platform::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SupportTierDto {
    Supported,
    Experimental,
    Unsupported,
}

impl From<SupportTier> for SupportTierDto {
    fn from(value: SupportTier) -> Self {
        match value {
            SupportTier::Supported => Self::Supported,
            SupportTier::Experimental => Self::Experimental,
            SupportTier::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CapabilityDto {
    Available,
    Unsupported {
        reason: String,
    },
    RequiresPermission {
        permission: PermissionKindDto,
        message: String,
    },
    RequiresExternalTool {
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        install_hint: Option<String>,
    },
    Experimental {
        message: String,
    },
}

impl From<Capability> for CapabilityDto {
    fn from(value: Capability) -> Self {
        match value {
            Capability::Available => Self::Available,
            Capability::Unsupported { reason } => Self::Unsupported { reason },
            Capability::RequiresPermission {
                permission,
                message,
            } => Self::RequiresPermission {
                permission: permission.into(),
                message,
            },
            Capability::RequiresExternalTool { tool, install_hint } => {
                Self::RequiresExternalTool { tool, install_hint }
            }
            Capability::Experimental { message } => Self::Experimental { message },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilitiesDto {
    pub platform: PlatformDto,
    pub tier: SupportTierDto,
    pub capture_text: CapabilityDto,
    pub capture_image: CapabilityDto,
    pub capture_files: CapabilityDto,
    pub write_text: CapabilityDto,
    pub write_image: CapabilityDto,
    pub clipboard_multi_representation_write: CapabilityDto,
    pub auto_paste: CapabilityDto,
    pub global_hotkey: CapabilityDto,
    pub frontmost_app: CapabilityDto,
    pub permissions_ui: CapabilityDto,
    pub update_check: CapabilityDto,
    pub preview_quick_look: CapabilityDto,
}

impl From<PlatformCapabilities> for PlatformCapabilitiesDto {
    fn from(value: PlatformCapabilities) -> Self {
        Self {
            platform: value.platform.into(),
            tier: value.tier.into(),
            capture_text: value.capture_text.into(),
            capture_image: value.capture_image.into(),
            capture_files: value.capture_files.into(),
            write_text: value.write_text.into(),
            write_image: value.write_image.into(),
            clipboard_multi_representation_write: value.clipboard_multi_representation_write.into(),
            auto_paste: value.auto_paste.into(),
            global_hotkey: value.global_hotkey.into(),
            frontmost_app: value.frontmost_app.into(),
            permissions_ui: value.permissions_ui.into(),
            update_check: value.update_check.into(),
            preview_quick_look: value.preview_quick_look.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AiProviderSettingDto {
    None,
    Local,
    Remote { name: String },
}

impl From<AiProviderSetting> for AiProviderSettingDto {
    fn from(value: AiProviderSetting) -> Self {
        match value {
            AiProviderSetting::None => Self::None,
            AiProviderSetting::Local => Self::Local,
            AiProviderSetting::Remote { name } => Self::Remote { name },
        }
    }
}

impl From<AiProviderSettingDto> for AiProviderSetting {
    fn from(value: AiProviderSettingDto) -> Self {
        match value {
            AiProviderSettingDto::None => Self::None,
            AiProviderSettingDto::Local => Self::Local,
            AiProviderSettingDto::Remote { name } => Self::Remote { name },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LocaleDto {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "ko")]
    Ko,
    #[serde(rename = "zh-Hans")]
    ZhHans,
    #[serde(rename = "zh-Hant")]
    ZhHant,
    #[serde(rename = "de")]
    De,
    #[serde(rename = "fr")]
    Fr,
    #[serde(rename = "es")]
    Es,
}

impl From<Locale> for LocaleDto {
    fn from(value: Locale) -> Self {
        match value {
            Locale::System => Self::System,
            Locale::En => Self::En,
            Locale::Ja => Self::Ja,
            Locale::Ko => Self::Ko,
            Locale::ZhHans => Self::ZhHans,
            Locale::ZhHant => Self::ZhHant,
            Locale::De => Self::De,
            Locale::Fr => Self::Fr,
            Locale::Es => Self::Es,
        }
    }
}

impl From<LocaleDto> for Locale {
    fn from(value: LocaleDto) -> Self {
        match value {
            LocaleDto::System => Self::System,
            LocaleDto::En => Self::En,
            LocaleDto::Ja => Self::Ja,
            LocaleDto::Ko => Self::Ko,
            LocaleDto::ZhHans => Self::ZhHans,
            LocaleDto::ZhHant => Self::ZhHant,
            LocaleDto::De => Self::De,
            LocaleDto::Fr => Self::Fr,
            LocaleDto::Es => Self::Es,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretHandlingDto {
    Block,
    StoreRedacted,
    StoreFull,
}

impl From<SecretHandling> for SecretHandlingDto {
    fn from(value: SecretHandling) -> Self {
        match value {
            SecretHandling::Block => Self::Block,
            SecretHandling::StoreRedacted => Self::StoreRedacted,
            SecretHandling::StoreFull => Self::StoreFull,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteFormatDto {
    Preserve,
    PlainText,
}

impl From<PasteFormat> for PasteFormatDto {
    fn from(value: PasteFormat) -> Self {
        match value {
            PasteFormat::Preserve => Self::Preserve,
            PasteFormat::PlainText => Self::PlainText,
        }
    }
}

impl From<PasteFormatDto> for PasteFormat {
    fn from(value: PasteFormatDto) -> Self {
        match value {
            PasteFormatDto::Preserve => Self::Preserve,
            PasteFormatDto::PlainText => Self::PlainText,
        }
    }
}

impl Default for PasteFormatDto {
    fn default() -> Self {
        PasteFormat::default().into()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecentOrderDto {
    ByRecency,
    ByUseCount,
    PinnedFirstThenRecency,
}

impl From<RecentOrder> for RecentOrderDto {
    fn from(value: RecentOrder) -> Self {
        match value {
            RecentOrder::ByRecency => Self::ByRecency,
            RecentOrder::ByUseCount => Self::ByUseCount,
            RecentOrder::PinnedFirstThenRecency => Self::PinnedFirstThenRecency,
        }
    }
}

impl From<RecentOrderDto> for RecentOrder {
    fn from(value: RecentOrderDto) -> Self {
        match value {
            RecentOrderDto::ByRecency => Self::ByRecency,
            RecentOrderDto::ByUseCount => Self::ByUseCount,
            RecentOrderDto::PinnedFirstThenRecency => Self::PinnedFirstThenRecency,
        }
    }
}

impl Default for RecentOrderDto {
    fn default() -> Self {
        RecentOrder::default().into()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceDto {
    Light,
    Dark,
    System,
}

impl From<Appearance> for AppearanceDto {
    fn from(value: Appearance) -> Self {
        match value {
            Appearance::Light => Self::Light,
            Appearance::Dark => Self::Dark,
            Appearance::System => Self::System,
        }
    }
}

impl From<AppearanceDto> for Appearance {
    fn from(value: AppearanceDto) -> Self {
        match value {
            AppearanceDto::Light => Self::Light,
            AppearanceDto::Dark => Self::Dark,
            AppearanceDto::System => Self::System,
        }
    }
}

impl Default for AppearanceDto {
    fn default() -> Self {
        Appearance::default().into()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannelDto {
    Stable,
}

impl From<UpdateChannel> for UpdateChannelDto {
    fn from(value: UpdateChannel) -> Self {
        match value {
            UpdateChannel::Stable => Self::Stable,
        }
    }
}

impl From<UpdateChannelDto> for UpdateChannel {
    fn from(value: UpdateChannelDto) -> Self {
        match value {
            UpdateChannelDto::Stable => Self::Stable,
        }
    }
}

impl Default for UpdateChannelDto {
    fn default() -> Self {
        UpdateChannel::default().into()
    }
}

impl From<SecretHandlingDto> for SecretHandling {
    fn from(value: SecretHandlingDto) -> Self {
        match value {
            SecretHandlingDto::Block => Self::Block,
            SecretHandlingDto::StoreRedacted => Self::StoreRedacted,
            SecretHandlingDto::StoreFull => Self::StoreFull,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsDto {
    pub global_hotkey: String,
    pub history_retention_count: usize,
    pub history_retention_days: Option<u32>,
    pub max_entry_size_bytes: usize,
    #[serde(default = "default_capture_kind_dtos")]
    pub capture_kinds: Vec<ContentKindDto>,
    pub max_total_bytes: Option<u64>,
    pub capture_enabled: bool,
    pub auto_paste_enabled: bool,
    #[serde(default)]
    pub paste_format_default: PasteFormatDto,
    pub paste_delay_ms: u64,
    pub app_denylist: Vec<String>,
    pub regex_denylist: Vec<String>,
    pub ai_provider: AiProviderSettingDto,
    pub ai_enabled: bool,
    pub semantic_search_enabled: bool,
    pub cli_ipc_enabled: bool,
    pub locale: LocaleDto,
    #[serde(default)]
    pub recent_order: RecentOrderDto,
    #[serde(default)]
    pub appearance: AppearanceDto,
    pub auto_launch: bool,
    #[serde(default)]
    pub secret_handling: SecretHandlingDto,
    #[serde(default)]
    pub palette_hotkeys: BTreeMap<PaletteHotkeyAction, String>,
    #[serde(default)]
    pub secondary_hotkeys: BTreeMap<SecondaryHotkeyAction, String>,
    #[serde(default = "nagori_core::settings::default_palette_row_count")]
    pub palette_row_count: u32,
    #[serde(default = "nagori_core::settings::default_show_preview_pane")]
    pub show_preview_pane: bool,
    #[serde(default = "nagori_core::settings::default_show_in_menu_bar")]
    pub show_in_menu_bar: bool,
    #[serde(default)]
    pub clear_on_quit: bool,
    #[serde(default = "nagori_core::settings::default_capture_initial_clipboard_on_launch")]
    pub capture_initial_clipboard_on_launch: bool,
    #[serde(default = "nagori_core::settings::default_auto_update_check")]
    pub auto_update_check: bool,
    #[serde(default)]
    pub update_channel: UpdateChannelDto,
    #[serde(default = "nagori_core::settings::default_max_thumbnail_total_bytes")]
    pub max_thumbnail_total_bytes: Option<u64>,
    /// Onboarding lifecycle markers (Phase A). `#[serde(default)]` keeps
    /// older settings snapshots forward-compatible — pre-Phase-A clients
    /// simply omit the field, which deserialises to all-`None`.
    #[serde(default)]
    pub onboarding: OnboardingSettingsDto,
}

/// Wire shape of [`OnboardingSettings`]. Mirrors the camelCase field
/// names already used elsewhere in the DTO surface so the renderer never
/// sees the `snake_case` core form.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
// `accessibility_*_at` / `completed_at` are timestamps by nature; the
// "all-fields-end-in-at" lint is noisier than useful here.
#[allow(clippy::struct_field_names)]
pub struct OnboardingSettingsDto {
    #[serde(with = "time::serde::rfc3339::option")]
    pub accessibility_prompted_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub accessibility_first_granted_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
}

impl From<OnboardingSettings> for OnboardingSettingsDto {
    fn from(value: OnboardingSettings) -> Self {
        Self {
            accessibility_prompted_at: value.accessibility_prompted_at,
            accessibility_first_granted_at: value.accessibility_first_granted_at,
            completed_at: value.completed_at,
        }
    }
}

impl From<OnboardingSettingsDto> for OnboardingSettings {
    fn from(value: OnboardingSettingsDto) -> Self {
        Self {
            accessibility_prompted_at: value.accessibility_prompted_at,
            accessibility_first_granted_at: value.accessibility_first_granted_at,
            completed_at: value.completed_at,
        }
    }
}

impl Default for SecretHandlingDto {
    fn default() -> Self {
        SecretHandling::default().into()
    }
}

impl From<AppSettings> for AppSettingsDto {
    fn from(value: AppSettings) -> Self {
        Self {
            global_hotkey: value.global_hotkey,
            history_retention_count: value.history_retention_count,
            history_retention_days: value.history_retention_days,
            max_entry_size_bytes: value.max_entry_size_bytes,
            capture_kinds: value.capture_kinds.into_iter().map(Into::into).collect(),
            max_total_bytes: value.max_total_bytes,
            capture_enabled: value.capture_enabled,
            auto_paste_enabled: value.auto_paste_enabled,
            paste_format_default: value.paste_format_default.into(),
            paste_delay_ms: value.paste_delay_ms,
            app_denylist: value.app_denylist,
            regex_denylist: value.regex_denylist,
            ai_provider: value.ai_provider.into(),
            ai_enabled: value.ai_enabled,
            semantic_search_enabled: value.semantic_search_enabled,
            cli_ipc_enabled: value.cli_ipc_enabled,
            locale: value.locale.into(),
            recent_order: value.recent_order.into(),
            appearance: value.appearance.into(),
            auto_launch: value.auto_launch,
            secret_handling: value.secret_handling.into(),
            palette_hotkeys: value.palette_hotkeys,
            secondary_hotkeys: value.secondary_hotkeys,
            palette_row_count: value.palette_row_count,
            show_preview_pane: value.show_preview_pane,
            show_in_menu_bar: value.show_in_menu_bar,
            clear_on_quit: value.clear_on_quit,
            capture_initial_clipboard_on_launch: value.capture_initial_clipboard_on_launch,
            auto_update_check: value.auto_update_check,
            update_channel: value.update_channel.into(),
            max_thumbnail_total_bytes: value.max_thumbnail_total_bytes,
            onboarding: value.onboarding.into(),
        }
    }
}

impl From<AppSettingsDto> for AppSettings {
    fn from(value: AppSettingsDto) -> Self {
        Self {
            global_hotkey: value.global_hotkey,
            history_retention_count: value.history_retention_count,
            history_retention_days: value.history_retention_days,
            max_entry_size_bytes: value.max_entry_size_bytes,
            capture_kinds: value.capture_kinds.into_iter().map(Into::into).collect(),
            max_total_bytes: value.max_total_bytes,
            capture_enabled: value.capture_enabled,
            auto_paste_enabled: value.auto_paste_enabled,
            paste_format_default: value.paste_format_default.into(),
            paste_delay_ms: value.paste_delay_ms,
            app_denylist: value.app_denylist,
            regex_denylist: value.regex_denylist,
            ai_provider: value.ai_provider.into(),
            ai_enabled: value.ai_enabled,
            semantic_search_enabled: value.semantic_search_enabled,
            cli_ipc_enabled: value.cli_ipc_enabled,
            locale: value.locale.into(),
            recent_order: value.recent_order.into(),
            appearance: value.appearance.into(),
            auto_launch: value.auto_launch,
            secret_handling: value.secret_handling.into(),
            palette_hotkeys: value.palette_hotkeys,
            secondary_hotkeys: value.secondary_hotkeys,
            palette_row_count: value.palette_row_count,
            show_preview_pane: value.show_preview_pane,
            show_in_menu_bar: value.show_in_menu_bar,
            clear_on_quit: value.clear_on_quit,
            capture_initial_clipboard_on_launch: value.capture_initial_clipboard_on_launch,
            auto_update_check: value.auto_update_check,
            update_channel: value.update_channel.into(),
            max_thumbnail_total_bytes: value.max_thumbnail_total_bytes,
            onboarding: value.onboarding.into(),
        }
    }
}

/// Current state of the bundled `nagori` CLI relative to the user's `PATH`.
/// Surfaced read-only in Settings → CLI so the "Install" button can render
/// the right affordance (install / re-link / already linked) without the
/// renderer probing the filesystem itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallStatusDto {
    /// Whether this OS build supports the one-click install at all. macOS and
    /// Linux symlink into `~/.local/bin`; Windows is `false` for now and the
    /// UI shows manual guidance instead.
    pub supported: bool,
    /// Whether the CLI binary actually shipped beside the desktop executable
    /// (false under `tauri dev`, where sidecars are not copied next to the
    /// dev binary).
    pub bundled: bool,
    /// Whether `<bin_dir>/nagori` already resolves to the bundled binary.
    pub installed: bool,
    /// Symlink destination this build would create / has created.
    pub installed_path: String,
    /// Directory the symlink lives in (`~/.local/bin`).
    pub bin_dir: String,
    /// Best-effort: whether `bin_dir` is on the user's shell `PATH`.
    pub on_path: bool,
}

/// Result of a successful `install_cli` call. Mirrors the status shape minus
/// the capability flags so the UI can confirm where the link landed and
/// whether the user still needs to extend their `PATH`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliInstallResultDto {
    /// Symlink that now points at the bundled binary.
    pub installed_path: String,
    /// Directory the symlink was created in (`~/.local/bin`).
    pub bin_dir: String,
    /// Bundled binary the symlink resolves to.
    pub source_path: String,
    /// Best-effort: whether `bin_dir` is on the user's shell `PATH`.
    pub on_path: bool,
}

#[cfg(test)]
mod tests {
    use nagori_core::{
        AppSettings, Appearance, ClipboardData, ClipboardRepresentation, ClipboardSnapshot,
        ContentKind, EntryFactory, PasteFormat, RecentOrder, SecretHandling, Sensitivity,
        UpdateChannel,
    };
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    fn text_entry(body: &str) -> nagori_core::ClipboardEntry {
        EntryFactory::from_text(body)
    }

    fn image_entry(bytes: Vec<u8>) -> nagori_core::ClipboardEntry {
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash(
                nagori_core::ContentHash::sha256(&bytes).value,
            ),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(bytes),
            }],
        };
        EntryFactory::from_snapshot(snapshot).expect("png snapshot should produce entry")
    }

    #[test]
    fn entry_preview_for_secret_text_only_exposes_redacted_preview() {
        let mut entry = text_entry("ghp_abcdefghijklmnopqrstuvwxyz1234567890");
        entry.search.preview = "[REDACTED]".to_owned();
        entry.sensitivity = Sensitivity::Secret;

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.sensitive);
        assert!(!dto.metadata.full_content_available);
        match dto.body {
            PreviewBodyDto::Text { ref text } => assert_eq!(text, "[REDACTED]"),
            other => panic!("expected redacted Text body, got {other:?}"),
        }
        assert_eq!(dto.preview_text, "[REDACTED]");
    }

    #[test]
    fn entry_preview_for_private_text_uses_preview_only() {
        let mut entry = text_entry("482915");
        entry.search.preview = "(redacted OTP)".to_owned();
        entry.sensitivity = Sensitivity::Private;

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.sensitive);
        match dto.body {
            PreviewBodyDto::Text { ref text } => assert_eq!(text, "(redacted OTP)"),
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_blocked_replaces_preview_with_placeholder() {
        // The classifier never sets `redacted_preview` for Blocked, so the
        // stored `search.preview` is still raw-derived. The DTO must
        // substitute the placeholder rather than surfacing whatever was on
        // the row, even when callers set it to a benign-looking string.
        let mut entry = text_entry("blocked clip");
        entry.search.preview = "raw secret value".to_owned();
        entry.sensitivity = Sensitivity::Blocked;

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.sensitive);
        assert!(!dto.metadata.full_content_available);
        match dto.body {
            PreviewBodyDto::Text { text } => {
                assert_eq!(text, nagori_core::BLOCKED_PREVIEW_PLACEHOLDER);
            }
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_image_returns_image_body_with_byte_count() {
        // The PNG magic prefix is required: `EntryFactory::from_snapshot`
        // drops image representations whose bytes don't match the
        // declared MIME, so a fake byte string would be rejected by the
        // capture-time signature gate before this test could observe a
        // preview.
        let bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0xCA, 0xFE];
        let entry = image_entry(bytes.clone());

        let dto = EntryPreviewDto::from_entry(&entry);
        match dto.body {
            PreviewBodyDto::Image {
                mime_type,
                byte_count,
                width,
                height,
            } => {
                assert_eq!(mime_type.as_deref(), Some("image/png"));
                assert_eq!(byte_count, bytes.len());
                assert_eq!(width, None);
                assert_eq!(height, None);
            }
            other => panic!("expected Image body, got {other:?}"),
        }
        assert!(matches!(dto.kind, ContentKindDto::Image));
    }

    #[test]
    fn entry_preview_head_and_tail_truncates_oversized_text_bodies() {
        // 200 KiB exceeds the 128 KiB preview cap. Head+tail truncation
        // keeps both ends visible with a middle sentinel that names the
        // elided byte count, so users can spot trailing context (closing
        // braces, footer signatures) that a head-only cut would lose.
        let body = "a".repeat(200 * 1024);
        let entry = text_entry(&body);

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.truncated);
        match dto.metadata.truncation {
            TruncationDto::HeadAndTail { elided_bytes } => {
                assert!(elided_bytes > 0);
                assert!(elided_bytes < body.len());
            }
            other => panic!("expected HeadAndTail truncation, got {other:?}"),
        }
        assert!(dto.preview_text.contains("bytes elided"));
        // Cap honoured (sentinel adds a few dozen bytes, allow modest slack).
        assert!(dto.preview_text.len() <= 128 * 1024 + 64);
        // Head and tail both reach the rendered body.
        assert!(dto.preview_text.starts_with('a'));
        assert!(dto.preview_text.ends_with('a'));
        // No query supplied → `elided_contains_match` stays `None` so the
        // renderer doesn't flag a missing hit when there is no search.
        assert!(dto.metadata.elided_contains_match.is_none());
    }

    #[test]
    fn entry_preview_head_and_tail_preserves_multibyte_char_boundaries() {
        // 4-byte emoji at both ends of the body. The truncator must split
        // on a char boundary so the rendered preview stays valid UTF-8
        // and the head/tail emojis survive.
        let chunk = "あ".repeat(50_000); // 3 bytes × 50_000 = 150 KiB
        let entry = text_entry(&chunk);
        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.truncated);
        // Round-trips as valid UTF-8 (no panic on `chars()`).
        assert!(dto.preview_text.chars().count() > 0);
        assert!(dto.preview_text.starts_with('あ'));
        assert!(dto.preview_text.ends_with('あ'));
    }

    #[test]
    fn entry_preview_below_caps_reports_truncation_none() {
        let entry = text_entry("hello world\nsecond line");
        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(!dto.metadata.truncated);
        assert!(matches!(dto.metadata.truncation, TruncationDto::None));
    }

    #[test]
    fn entry_preview_with_query_flags_elided_match_only_when_hit_is_hidden() {
        // Place the marker in the elided middle. With ~200 KiB total
        // body and a 128 KiB cap, the kept head/tail are ≈64 KiB each
        // and the marker (at byte ~100_000) lands inside the cut.
        let mut body = String::with_capacity(200 * 1024);
        body.push_str(&"x".repeat(100_000));
        body.push_str("NEEDLE-IN-THE-HAYSTACK");
        body.push_str(&"y".repeat(100_000));
        let entry = text_entry(&body);
        let with_match =
            EntryPreviewDto::from_entry_with_query(&entry, Some("NEEDLE-IN-THE-HAYSTACK"));
        assert_eq!(with_match.metadata.elided_contains_match, Some(true));
        let with_other =
            EntryPreviewDto::from_entry_with_query(&entry, Some("not-in-this-document"));
        assert_eq!(with_other.metadata.elided_contains_match, Some(false));
        // Empty / whitespace queries are treated as "no query" so the
        // renderer never emits a spurious warning on an empty palette.
        let with_empty = EntryPreviewDto::from_entry_with_query(&entry, Some("   "));
        assert!(with_empty.metadata.elided_contains_match.is_none());
    }

    #[test]
    fn entry_preview_with_query_normalizes_case_and_terms_against_elided_region() {
        // The FTS pipeline lowercases via `normalize_text`; the hint must
        // follow the same normalization so a case-mismatched query still
        // surfaces the warning when the hit hides in the middle.
        let mut body = String::with_capacity(200 * 1024);
        body.push_str(&"x".repeat(100_000));
        body.push_str("HiddenKeyword");
        body.push_str(&"y".repeat(100_000));
        let entry = text_entry(&body);
        // Different case from the body — raw contains() would miss this.
        let lowered = EntryPreviewDto::from_entry_with_query(&entry, Some("hiddenkeyword"));
        assert_eq!(lowered.metadata.elided_contains_match, Some(true));
        // Multi-term query: both tokens must hit the region (all-of-terms).
        let mut body2 = String::with_capacity(200 * 1024);
        body2.push_str(&"x".repeat(100_000));
        body2.push_str("foo bar baz");
        body2.push_str(&"y".repeat(100_000));
        let entry2 = text_entry(&body2);
        let both = EntryPreviewDto::from_entry_with_query(&entry2, Some("foo BAR"));
        assert_eq!(both.metadata.elided_contains_match, Some(true));
        let one_missing = EntryPreviewDto::from_entry_with_query(&entry2, Some("foo qux"));
        assert_eq!(one_missing.metadata.elided_contains_match, Some(false));
    }

    #[test]
    fn entry_preview_full_content_available_tracks_public_only() {
        // `full_content_available` must mirror the IPC gate on
        // `get_entry_preview_full` (Public-only) so the expand button is
        // never offered for entries that would be rejected at invoke time.
        // `EntryFactory::from_text` returns `Unknown` by default, so the
        // base fixture is already a non-Public case.
        let unknown = text_entry("hello world");
        assert_eq!(unknown.sensitivity, Sensitivity::Unknown);
        let unk_dto = EntryPreviewDto::from_entry(&unknown);
        // `Unknown` is text-safe for default DTOs but not Public, so the
        // expand affordance must stay off.
        assert!(!unk_dto.metadata.full_content_available);

        let mut public = text_entry("hello world");
        public.sensitivity = Sensitivity::Public;
        let pub_dto = EntryPreviewDto::from_entry(&public);
        assert!(pub_dto.metadata.full_content_available);

        let mut secret = text_entry("hello world");
        secret.sensitivity = Sensitivity::Secret;
        let sec_dto = EntryPreviewDto::from_entry(&secret);
        assert!(!sec_dto.metadata.full_content_available);
    }

    #[test]
    fn entry_preview_with_query_short_body_emits_no_elided_hint() {
        // Body fits in the cap; nothing was elided so the flag must stay
        // `None` rather than `Some(false)` (no region to inspect).
        let entry = text_entry("alpha beta gamma");
        let dto = EntryPreviewDto::from_entry_with_query(&entry, Some("delta"));
        assert!(dto.metadata.elided_contains_match.is_none());
    }

    #[test]
    fn entry_preview_full_uses_higher_byte_cap_than_default() {
        // 256 KiB body exceeds the standard 128 KiB cap but fits inside
        // the 1 MiB expanded cap, so the expanded path returns the body
        // untruncated while the default path falls back to head+tail.
        let body = "a".repeat(256 * 1024);
        let entry = text_entry(&body);
        let standard = EntryPreviewDto::from_entry(&entry);
        assert!(standard.metadata.truncated);
        let expanded = EntryPreviewDto::from_entry_full(&entry);
        assert!(!expanded.metadata.truncated);
        assert!(matches!(expanded.metadata.truncation, TruncationDto::None));
        assert_eq!(expanded.preview_text.len(), body.len());
    }

    #[test]
    fn entry_preview_truncates_by_line_count_with_head_and_tail() {
        // A short-line body that beats the byte cap purely on line count
        // (5,000 lines × 4 bytes = 20 KiB). The line-cap path must kick
        // in before the byte cap and emit the head+tail sentinel.
        let body: String = (0..5_000)
            .map(|i| format!("ln{i}\n"))
            .collect::<Vec<_>>()
            .concat();
        let entry = text_entry(&body);
        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.truncated);
        assert!(matches!(
            dto.metadata.truncation,
            TruncationDto::HeadAndTail { .. }
        ));
        assert!(dto.preview_text.contains("lines elided"));
        // First and last lines survive.
        assert!(dto.preview_text.starts_with("ln0\n"));
        assert!(dto.preview_text.contains("ln4999"));
    }

    #[test]
    fn entry_preview_for_file_list_caps_paths_but_reports_total() {
        // Frontend renders `paths.len() / total` and a "+N more files" hint
        // when the underlying clip exceeds the 50-path wire cap. Ensure the
        // DTO carries the pre-truncation count rather than the truncated
        // `paths.len()`.
        let paths: Vec<String> = (0..75).map(|i| format!("/tmp/file-{i:03}.txt")).collect();
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash("fl-many"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(paths),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("file list snapshot");
        let dto = EntryPreviewDto::from_entry(&entry);
        match dto.body {
            PreviewBodyDto::FileList {
                paths: wire_paths,
                total,
            } => {
                assert_eq!(wire_paths.len(), 50);
                assert_eq!(total, 75);
                assert_eq!(
                    wire_paths.first().map(String::as_str),
                    Some("/tmp/file-000.txt")
                );
            }
            other => panic!("expected FileList body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_short_file_list_reports_total_equal_to_paths() {
        let paths = vec!["/tmp/a.txt".to_owned(), "/tmp/b.txt".to_owned()];
        let expected_len = paths.len();
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash("fl-short"),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(paths.clone()),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("file list snapshot");
        let dto = EntryPreviewDto::from_entry(&entry);
        match dto.body {
            PreviewBodyDto::FileList {
                paths: wire_paths,
                total,
            } => {
                assert_eq!(wire_paths, paths);
                assert_eq!(total, expected_len);
            }
            other => panic!("expected FileList body, got {other:?}"),
        }
    }

    #[test]
    fn app_settings_dto_round_trip_preserves_every_field() {
        use nagori_core::{PaletteHotkeyAction, SecondaryHotkeyAction};
        use std::collections::BTreeMap;
        // Pin every field so a future addition that forgets one of the
        // conversion arms (camelCase serde rename, secret_handling default,
        // ai_provider variants, locale tag) trips this test.
        let mut palette_hotkeys = BTreeMap::new();
        palette_hotkeys.insert(PaletteHotkeyAction::Pin, "Cmd+Alt+P".to_owned());
        let mut secondary_hotkeys = BTreeMap::new();
        secondary_hotkeys.insert(SecondaryHotkeyAction::RepasteLast, "Cmd+Alt+V".to_owned());

        let original = AppSettings {
            global_hotkey: "Cmd+Shift+V".to_owned(),
            history_retention_count: 1234,
            history_retention_days: Some(7),
            max_entry_size_bytes: 2 * 1024 * 1024,
            capture_kinds: [ContentKind::Text, ContentKind::Image]
                .into_iter()
                .collect(),
            max_total_bytes: Some(64 * 1024 * 1024),
            capture_enabled: false,
            auto_paste_enabled: true,
            paste_format_default: PasteFormat::PlainText,
            paste_delay_ms: 80,
            app_denylist: vec!["1Password".to_owned(), "Bitwarden".to_owned()],
            regex_denylist: vec!["INTERNAL-\\d+".to_owned()],
            ai_provider: AiProviderSetting::Remote {
                name: "anthropic".to_owned(),
            },
            ai_enabled: true,
            semantic_search_enabled: true,
            cli_ipc_enabled: false,
            locale: nagori_core::Locale::Ja,
            recent_order: RecentOrder::ByUseCount,
            appearance: Appearance::Dark,
            auto_launch: true,
            secret_handling: SecretHandling::StoreFull,
            palette_hotkeys: palette_hotkeys.clone(),
            secondary_hotkeys: secondary_hotkeys.clone(),
            palette_row_count: 12,
            show_preview_pane: false,
            show_in_menu_bar: false,
            clear_on_quit: true,
            capture_initial_clipboard_on_launch: false,
            auto_update_check: false,
            update_channel: UpdateChannel::Stable,
            max_thumbnail_total_bytes: Some(32 * 1024 * 1024),
            onboarding: nagori_core::settings::OnboardingSettings {
                accessibility_prompted_at: Some(OffsetDateTime::UNIX_EPOCH),
                accessibility_first_granted_at: None,
                completed_at: None,
            },
        };

        let dto: AppSettingsDto = original.clone().into();
        let restored: AppSettings = dto.into();
        assert_eq!(restored.global_hotkey, original.global_hotkey);
        assert_eq!(
            restored.history_retention_count,
            original.history_retention_count
        );
        assert_eq!(
            restored.history_retention_days,
            original.history_retention_days
        );
        assert_eq!(restored.max_entry_size_bytes, original.max_entry_size_bytes);
        assert_eq!(restored.capture_kinds, original.capture_kinds);
        assert_eq!(restored.max_total_bytes, original.max_total_bytes);
        assert_eq!(restored.capture_enabled, original.capture_enabled);
        assert_eq!(restored.auto_paste_enabled, original.auto_paste_enabled);
        assert_eq!(restored.paste_format_default, original.paste_format_default);
        assert_eq!(restored.paste_delay_ms, original.paste_delay_ms);
        assert_eq!(restored.app_denylist, original.app_denylist);
        assert_eq!(restored.regex_denylist, original.regex_denylist);
        assert!(matches!(
            restored.ai_provider,
            AiProviderSetting::Remote { ref name } if name == "anthropic",
        ));
        assert_eq!(restored.ai_enabled, original.ai_enabled);
        assert_eq!(
            restored.semantic_search_enabled,
            original.semantic_search_enabled
        );
        assert_eq!(restored.cli_ipc_enabled, original.cli_ipc_enabled);
        assert!(matches!(restored.locale, nagori_core::Locale::Ja));
        assert!(matches!(restored.recent_order, RecentOrder::ByUseCount));
        assert!(matches!(restored.appearance, Appearance::Dark));
        assert_eq!(restored.auto_launch, original.auto_launch);
        assert!(matches!(
            restored.secret_handling,
            SecretHandling::StoreFull
        ));
        assert_eq!(restored.palette_hotkeys, palette_hotkeys);
        assert_eq!(restored.secondary_hotkeys, secondary_hotkeys);
        assert_eq!(restored.palette_row_count, 12);
        assert!(!restored.show_preview_pane);
        assert!(!restored.show_in_menu_bar);
        assert!(restored.clear_on_quit);
        assert!(!restored.capture_initial_clipboard_on_launch);
        assert!(!restored.auto_update_check);
        assert!(matches!(restored.update_channel, UpdateChannel::Stable));
    }

    #[test]
    fn onboarding_dto_serialises_as_camel_case_rfc3339() {
        // The frontend reads `onboarding.accessibilityPromptedAt` etc.
        // as RFC3339 strings (or `null`). Pin both the camelCase rename
        // and the RFC3339 serialisation so a future serde tweak on the
        // `time::serde::rfc3339::option` adapter cannot silently break
        // the wire format. Also asserts the absent marker emits `null`
        // rather than being skipped — the TS contract treats absence as
        // a JSON parsing error.
        let stamped =
            OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("static timestamp parses");
        let core = nagori_core::OnboardingSettings {
            accessibility_prompted_at: Some(stamped),
            accessibility_first_granted_at: None,
            completed_at: None,
        };
        let dto: OnboardingSettingsDto = core.clone().into();
        let json = serde_json::to_value(&dto).expect("serialise");
        assert_eq!(
            json["accessibilityPromptedAt"],
            json!("2023-11-14T22:13:20Z")
        );
        assert_eq!(json["accessibilityFirstGrantedAt"], json!(null));
        assert_eq!(json["completedAt"], json!(null));
        // snake_case must not coexist with camelCase rename.
        assert!(
            json.get("accessibility_prompted_at").is_none() && json.get("completed_at").is_none(),
            "snake_case fields must not appear on the wire",
        );
        // Round-trip the JSON back through the DTO and into the core
        // type so the timestamp survives the conversion.
        let parsed: OnboardingSettingsDto =
            serde_json::from_value(json).expect("deserialise OnboardingSettingsDto");
        let restored: nagori_core::OnboardingSettings = parsed.into();
        assert_eq!(restored, core);
    }

    #[test]
    fn app_settings_dto_serializes_secret_handling_as_snake_case() {
        // The Tauri command boundary speaks JSON — the Svelte side reads
        // `secret_handling: "store_redacted"`, so the snake_case rename must
        // survive any future churn on the enum.
        let dto: AppSettingsDto = AppSettings::default().into();
        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(json["secretHandling"], json!("store_redacted"));
        assert_eq!(json["aiProvider"], json!("none"));
        assert_eq!(json["locale"], json!("system"));
        assert_eq!(json["pasteFormatDefault"], json!("preserve"));
        assert_eq!(json["recentOrder"], json!("by_recency"));
        assert_eq!(json["appearance"], json!("system"));
    }

    #[test]
    fn locale_dto_wire_tag_is_stable_for_every_variant() {
        // The frontend parses the locale tag verbatim — a typo in a serde
        // rename would silently drop a locale even though the type-level
        // `From` arms still match. Pin the wire format for every variant.
        let cases: &[(nagori_core::Locale, &str)] = &[
            (nagori_core::Locale::System, "system"),
            (nagori_core::Locale::En, "en"),
            (nagori_core::Locale::Ja, "ja"),
            (nagori_core::Locale::Ko, "ko"),
            (nagori_core::Locale::ZhHans, "zh-Hans"),
            (nagori_core::Locale::ZhHant, "zh-Hant"),
            (nagori_core::Locale::De, "de"),
            (nagori_core::Locale::Fr, "fr"),
            (nagori_core::Locale::Es, "es"),
        ];
        for (locale, expected) in cases {
            let dto: LocaleDto = (*locale).into();
            let serialized = serde_json::to_value(dto).expect("serialize");
            assert_eq!(serialized, json!(expected), "wire tag for {locale:?}");
            let parsed: LocaleDto = serde_json::from_value(json!(expected)).expect("deserialize");
            let round_tripped: nagori_core::Locale = parsed.into();
            assert_eq!(round_tripped, *locale, "round-trip for {locale:?}");
        }
    }

    #[test]
    fn url_preview_serialises_fields_in_camel_case() {
        // The Url variant's structured fields cross the IPC wire to
        // the TS renderer as camelCase. `rename_all = "camelCase"` on
        // the enum only renames variant names, so without the matching
        // `rename_all_fields` the new structured fields would ship as
        // snake_case and silently break the renderer fallback (the host
        // row would always read from `url`, the punycode badge would
        // never fire). Lock this here so a future serde refactor cannot
        // regress the contract.
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash(
                nagori_core::ContentHash::sha256(b"https://example.com/foo").value,
            ),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("https://example.com/foo?bar=1".to_owned()),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("url snapshot");
        let dto = EntryPreviewDto::from_entry(&entry);
        let json = serde_json::to_value(&dto.body).expect("serialise url body");
        assert_eq!(json["type"], serde_json::json!("url"));
        assert_eq!(json["hostDisplay"], serde_json::json!("example.com"));
        assert_eq!(json["pathAndQuery"], serde_json::json!("/foo?bar=1"));
        assert_eq!(json["scheme"], serde_json::json!("https"));
        assert!(
            json.get("host_display").is_none() && json.get("path_and_query").is_none(),
            "snake_case fields must not coexist with camelCase rename"
        );
    }

    #[test]
    fn entry_preview_for_url_emits_url_body_with_domain() {
        // URL-shaped clips should round-trip the parsed domain so the
        // frontend can render the badged preview without re-parsing.
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash(
                nagori_core::ContentHash::sha256(b"https://example.com/foo").value,
            ),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("https://example.com/foo?bar=1".to_owned()),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("url snapshot");
        let dto = EntryPreviewDto::from_entry(&entry);
        match dto.body {
            PreviewBodyDto::Url {
                url,
                domain,
                scheme,
                host_display,
                host_punycode,
                path_and_query,
            } => {
                assert!(url.contains("example.com"));
                assert_eq!(domain.as_deref(), Some("example.com"));
                assert_eq!(scheme.as_deref(), Some("https"));
                assert_eq!(host_display.as_deref(), Some("example.com"));
                assert!(host_punycode.is_none(), "ASCII host must omit punycode");
                assert_eq!(path_and_query.as_deref(), Some("/foo?bar=1"));
            }
            other => panic!("expected Url body, got {other:?}"),
        }
    }

    #[test]
    fn url_parts_flag_idn_host_with_punycode_badge() {
        // IDN hosts get a Unicode display row and the xn-- ASCII form
        // surfaces so the renderer can warn about homograph attacks.
        let parts = UrlParts::from_raw("https://xn--bcher-kva.example/").expect("idn parses");
        assert_eq!(parts.scheme, "https");
        assert_eq!(parts.host_display, "bücher.example");
        assert_eq!(
            parts.host_punycode.as_deref(),
            Some("xn--bcher-kva.example")
        );
        assert_eq!(parts.path_and_query, "/");
    }

    #[test]
    fn url_parts_reject_non_url_bodies() {
        // Non-URL strings (no scheme + host) must fall through to the flat
        // `url` render rather than producing a partially-populated split.
        assert!(UrlParts::from_raw("not a url").is_none());
        assert!(UrlParts::from_raw("mailto:user@example.com").is_none());
    }

    #[test]
    fn url_parts_surface_non_default_port_in_host_display() {
        // Non-default ports must appear in `host_display` (and in the
        // punycode badge value when set) so the confirm modal can't hide
        // a redirect to `:8443` behind a familiar-looking hostname.
        let parts = UrlParts::from_raw("https://example.com:8443/admin").expect("parses");
        assert_eq!(parts.host_display, "example.com:8443");
        assert!(parts.host_punycode.is_none());

        let idn = UrlParts::from_raw("https://xn--bcher-kva.example:8443/").expect("idn parses");
        assert_eq!(idn.host_display, "bücher.example:8443");
        assert_eq!(
            idn.host_punycode.as_deref(),
            Some("xn--bcher-kva.example:8443")
        );

        // Default port for the scheme is collapsed by `url::Url`, so it
        // does not leak into the display row.
        let default_port = UrlParts::from_raw("https://example.com:443/").expect("parses");
        assert_eq!(default_port.host_display, "example.com");
    }

    #[test]
    fn entry_dto_omits_text_for_private_or_secret_unless_caller_opts_in() {
        // `EntryDto::from_entry` exposes the `include_text` flag so the
        // command layer can keep raw bodies out of the default response shape
        // for sensitive entries while still returning text on copy/paste paths.
        let mut entry = text_entry("super secret value");
        entry.sensitivity = Sensitivity::Secret;

        let stripped = EntryDto::from_entry(entry.clone(), false);
        assert!(stripped.text.is_none());
        let with_text = EntryDto::from_entry(entry, true);
        assert_eq!(with_text.text.as_deref(), Some("super secret value"));
    }

    #[test]
    fn capability_dto_serializes_struct_variant_fields_in_camel_case() {
        // `rename_all = "camelCase"` only touches variant names — without
        // `rename_all_fields` the inner `install_hint` ships as snake_case
        // and silently de-syncs from the TS `installHint?` contract.
        let dto = CapabilityDto::from(nagori_platform::Capability::RequiresExternalTool {
            tool: "wtype".to_owned(),
            install_hint: Some("apt install wtype".to_owned()),
        });
        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(json["status"], json!("requiresExternalTool"));
        assert_eq!(json["tool"], json!("wtype"));
        assert_eq!(json["installHint"], json!("apt install wtype"));
        assert!(
            json.get("install_hint").is_none(),
            "snake_case field should not coexist with camelCase rename"
        );
    }
}

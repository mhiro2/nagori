use nagori_core::{
    ClipboardContent, ClipboardEntry, EntryId, Sensitivity, extension_of, find_common_parent,
    fold_home, is_text_safe_for_default_output, normalize_text, parent_for_display,
    safe_preview_for_dto, split_path,
};
use serde::Serialize;

use super::ContentKindDto;

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
        // Per-file basename-first decomposition, capped at
        // `FILE_LIST_PREVIEW_CAP`. The renderer leads with the basename and
        // drops the location to its own row instead of re-splitting a raw path
        // string, so the preview no longer parses paths client-side.
        entries: Vec<FileEntryDto>,
        // Pre-truncation file count. `entries` is capped so the renderer can
        // show `entries.length / total` without re-counting and surface a
        // "+N more" hint when the underlying clipboard list is longer.
        total: usize,
        // Home-folded longest directory prefix shared by every path (the full
        // list, not just the capped `entries`), with its trailing separator
        // stripped. The renderer hoists it into a single header and shows each
        // row relative to it; `None` when the paths span unrelated trees or
        // share only a lone filesystem root.
        #[serde(skip_serializing_if = "Option::is_none")]
        common_parent_display: Option<String>,
    },
    RichText {
        text: String,
    },
    Unknown {
        text: String,
    },
}

/// One file in a `FileList` preview body, decomposed basename-first. Only built
/// for bodies that already passed the default-output gate (sensitive entries
/// degrade to a `Text` body upstream), so the raw `parent_raw` it may carry is
/// no broader than the raw `paths` such a body used to ship.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntryDto {
    /// Final path segment, with a trailing separator re-attached for a
    /// directory entry (`build/`) so the row reads as a folder.
    pub name: String,
    /// Parent directory, home-folded to `~` and stripped of its trailing
    /// separator (a filesystem root stays intact). Empty when the path has no
    /// parent segment. This is the display string the Location row and the
    /// row's accessible name read from.
    pub parent_display: String,
    /// The same parent before home folding, surfaced only as a hover `title`
    /// so the un-folded absolute location is still recoverable. Absent when the
    /// path has no parent or the raw form already equals `parent_display`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_raw: Option<String>,
    /// Lowercased filename extension, absent for dotfiles / extensionless names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// File-vs-directory kind. Always `Unknown` today: captured paths carry no
    /// reliable kind signal, so the field is a forward-compat slot the renderer
    /// can lean on once a platform API supplies one.
    pub kind: FileEntryKindDto,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
// `File` / `Directory` are reserved wire values: captured paths carry no
// reliable kind signal today, so the builder only ever emits `Unknown`. They
// stay in the contract (and the mirrored TS union) so a future platform probe
// can populate them without a wire change — hence the allow.
#[allow(dead_code)]
pub enum FileEntryKindDto {
    File,
    Directory,
    Unknown,
}

/// Upper bound on the per-file rows shipped in a `FileList` preview body. The
/// total count is reported separately so the renderer can show a "+N more" hint.
const FILE_LIST_PREVIEW_CAP: usize = 50;

/// Decompose a captured file list into basename-first rows plus the home-folded
/// directory prefix they share. `paths` is the full list (sensitivity already
/// cleared by the caller); `home` folds the current user's home to `~`.
fn build_file_entries(paths: &[String], home: Option<&str>) -> (Vec<FileEntryDto>, Option<String>) {
    let entries = paths
        .iter()
        .take(FILE_LIST_PREVIEW_CAP)
        .map(|path| {
            let split = split_path(path);
            let name = format!("{}{}", split.base, split.trailing);
            let extension = extension_of(&split.base);
            let (parent_display, parent_raw) = if split.dir.is_empty() {
                (String::new(), None)
            } else {
                let raw = parent_for_display(&split.dir);
                let display = fold_home(&raw, home);
                // Only carry the raw parent when folding actually changed it,
                // so the hover title is additive rather than redundant.
                let parent_raw = (display != raw).then(|| raw.clone());
                (display, parent_raw)
            };
            FileEntryDto {
                name,
                parent_display,
                parent_raw,
                extension,
                kind: FileEntryKindDto::Unknown,
            }
        })
        .collect();
    let common = find_common_parent(paths);
    let common_parent_display =
        (!common.is_empty()).then(|| fold_home(&parent_for_display(&common), home));
    (entries, common_parent_display)
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
    // `from_entry_with_query` so the elided-match hint can flow through. `home`
    // folds the current user's home directory to `~` in `FileList` locations
    // (`None` leaves absolute paths verbatim).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_entry(entry: &ClipboardEntry, home: Option<&str>) -> Self {
        Self::build(entry, MAX_PREVIEW_BYTES, None, home)
    }

    /// Same as `from_entry` but tags `elided_contains_match` when the
    /// supplied search query (raw user input — not normalised) appears in
    /// the middle region we just elided. Empty queries are treated as
    /// "no query" so the renderer never emits a misleading warning on a
    /// pristine preview pane.
    pub fn from_entry_with_query(
        entry: &ClipboardEntry,
        query: Option<&str>,
        home: Option<&str>,
    ) -> Self {
        let trimmed = query.map(str::trim).filter(|q| !q.is_empty());
        Self::build(entry, MAX_PREVIEW_BYTES, trimmed, home)
    }

    /// Build a preview with a larger byte cap (used by `get_entry_preview_full`).
    /// Sensitive entries are still redacted to the safe-preview placeholder
    /// at the caller; this method does not relax sensitivity gating.
    pub fn from_entry_full(entry: &ClipboardEntry, home: Option<&str>) -> Self {
        Self::build(entry, MAX_PREVIEW_FULL_BYTES, None, home)
    }

    fn build(
        entry: &ClipboardEntry,
        byte_cap: usize,
        query: Option<&str>,
        home: Option<&str>,
    ) -> Self {
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
                ClipboardContent::FileList(value) => {
                    let (entries, common_parent_display) = build_file_entries(&value.paths, home);
                    PreviewBodyDto::FileList {
                        entries,
                        total: value.paths.len(),
                        common_parent_display,
                    }
                }
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

#[cfg(test)]
mod tests {
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSnapshot, EntryFactory, Sensitivity,
    };
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

        let dto = EntryPreviewDto::from_entry(&entry, None);
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

        let dto = EntryPreviewDto::from_entry(&entry, None);
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

        let dto = EntryPreviewDto::from_entry(&entry, None);
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

        let dto = EntryPreviewDto::from_entry(&entry, None);
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

        let dto = EntryPreviewDto::from_entry(&entry, None);
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
        let dto = EntryPreviewDto::from_entry(&entry, None);
        assert!(dto.metadata.truncated);
        // Round-trips as valid UTF-8 (no panic on `chars()`).
        assert!(dto.preview_text.chars().count() > 0);
        assert!(dto.preview_text.starts_with('あ'));
        assert!(dto.preview_text.ends_with('あ'));
    }

    #[test]
    fn entry_preview_below_caps_reports_truncation_none() {
        let entry = text_entry("hello world\nsecond line");
        let dto = EntryPreviewDto::from_entry(&entry, None);
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
            EntryPreviewDto::from_entry_with_query(&entry, Some("NEEDLE-IN-THE-HAYSTACK"), None);
        assert_eq!(with_match.metadata.elided_contains_match, Some(true));
        let with_other =
            EntryPreviewDto::from_entry_with_query(&entry, Some("not-in-this-document"), None);
        assert_eq!(with_other.metadata.elided_contains_match, Some(false));
        // Empty / whitespace queries are treated as "no query" so the
        // renderer never emits a spurious warning on an empty palette.
        let with_empty = EntryPreviewDto::from_entry_with_query(&entry, Some("   "), None);
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
        let lowered = EntryPreviewDto::from_entry_with_query(&entry, Some("hiddenkeyword"), None);
        assert_eq!(lowered.metadata.elided_contains_match, Some(true));
        // Multi-term query: both tokens must hit the region (all-of-terms).
        let mut body2 = String::with_capacity(200 * 1024);
        body2.push_str(&"x".repeat(100_000));
        body2.push_str("foo bar baz");
        body2.push_str(&"y".repeat(100_000));
        let entry2 = text_entry(&body2);
        let both = EntryPreviewDto::from_entry_with_query(&entry2, Some("foo BAR"), None);
        assert_eq!(both.metadata.elided_contains_match, Some(true));
        let one_missing = EntryPreviewDto::from_entry_with_query(&entry2, Some("foo qux"), None);
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
        let unk_dto = EntryPreviewDto::from_entry(&unknown, None);
        // `Unknown` is text-safe for default DTOs but not Public, so the
        // expand affordance must stay off.
        assert!(!unk_dto.metadata.full_content_available);

        let mut public = text_entry("hello world");
        public.sensitivity = Sensitivity::Public;
        let pub_dto = EntryPreviewDto::from_entry(&public, None);
        assert!(pub_dto.metadata.full_content_available);

        let mut secret = text_entry("hello world");
        secret.sensitivity = Sensitivity::Secret;
        let sec_dto = EntryPreviewDto::from_entry(&secret, None);
        assert!(!sec_dto.metadata.full_content_available);
    }

    #[test]
    fn entry_preview_with_query_short_body_emits_no_elided_hint() {
        // Body fits in the cap; nothing was elided so the flag must stay
        // `None` rather than `Some(false)` (no region to inspect).
        let entry = text_entry("alpha beta gamma");
        let dto = EntryPreviewDto::from_entry_with_query(&entry, Some("delta"), None);
        assert!(dto.metadata.elided_contains_match.is_none());
    }

    #[test]
    fn entry_preview_full_uses_higher_byte_cap_than_default() {
        // 256 KiB body exceeds the standard 128 KiB cap but fits inside
        // the 1 MiB expanded cap, so the expanded path returns the body
        // untruncated while the default path falls back to head+tail.
        let body = "a".repeat(256 * 1024);
        let entry = text_entry(&body);
        let standard = EntryPreviewDto::from_entry(&entry, None);
        assert!(standard.metadata.truncated);
        let expanded = EntryPreviewDto::from_entry_full(&entry, None);
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
        let dto = EntryPreviewDto::from_entry(&entry, None);
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

    fn file_list_entry(paths: Vec<String>, sequence: &str) -> nagori_core::ClipboardEntry {
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash(sequence),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(paths),
            }],
        };
        EntryFactory::from_snapshot(snapshot).expect("file list snapshot")
    }

    #[test]
    fn entry_preview_for_file_list_caps_entries_but_reports_total() {
        // Frontend renders `entries.len() / total` and a "+N more files" hint
        // when the underlying clip exceeds the per-row cap. Ensure the DTO
        // carries the pre-truncation count rather than the truncated length,
        // while the shared common parent reflects the whole list.
        let paths: Vec<String> = (0..75).map(|i| format!("/tmp/file-{i:03}.txt")).collect();
        let entry = file_list_entry(paths, "fl-many");
        let dto = EntryPreviewDto::from_entry(&entry, None);
        match dto.body {
            PreviewBodyDto::FileList {
                entries,
                total,
                common_parent_display,
            } => {
                assert_eq!(entries.len(), 50);
                assert_eq!(total, 75);
                assert_eq!(entries[0].name, "file-000.txt");
                assert_eq!(entries[0].parent_display, "/tmp");
                assert_eq!(common_parent_display.as_deref(), Some("/tmp"));
            }
            other => panic!("expected FileList body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_short_file_list_reports_total_equal_to_entries() {
        let entry = file_list_entry(
            vec!["/tmp/a.txt".to_owned(), "/tmp/b.txt".to_owned()],
            "fl-short",
        );
        let dto = EntryPreviewDto::from_entry(&entry, None);
        match dto.body {
            PreviewBodyDto::FileList {
                entries,
                total,
                common_parent_display,
            } => {
                assert_eq!(total, 2);
                let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
                assert_eq!(names, vec!["a.txt", "b.txt"]);
                assert_eq!(common_parent_display.as_deref(), Some("/tmp"));
            }
            other => panic!("expected FileList body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_file_list_folds_home_and_omits_redundant_raw_parent() {
        // A single file under the user's home folds its location to `~/…` for
        // display while keeping the un-folded absolute parent as the raw hover
        // disclosure. A path outside home leaves `parent_raw` absent because
        // the display string is already the raw one.
        let entry = file_list_entry(
            vec![
                "/Users/ex/Documents/report.pptx".to_owned(),
                "/opt/data/build.log".to_owned(),
            ],
            "fl-home",
        );
        let dto = EntryPreviewDto::from_entry(&entry, Some("/Users/ex"));
        match dto.body {
            PreviewBodyDto::FileList {
                entries,
                common_parent_display,
                ..
            } => {
                assert_eq!(entries[0].parent_display, "~/Documents");
                assert_eq!(
                    entries[0].parent_raw.as_deref(),
                    Some("/Users/ex/Documents")
                );
                assert_eq!(entries[1].parent_display, "/opt/data");
                assert!(entries[1].parent_raw.is_none());
                // No shared tree beyond the root, so no hoisted header.
                assert!(common_parent_display.is_none());
            }
            other => panic!("expected FileList body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_directory_entry_reattaches_trailing_separator() {
        // A directory entry keeps its trailing separator on `name` so the row
        // reads as a folder, and the extension stays absent.
        let entry = file_list_entry(
            vec!["/proj/build/".to_owned(), "/proj/build/file.txt".to_owned()],
            "fl-dir",
        );
        let dto = EntryPreviewDto::from_entry(&entry, None);
        match dto.body {
            PreviewBodyDto::FileList {
                entries,
                common_parent_display,
                ..
            } => {
                assert_eq!(entries[0].name, "build/");
                assert!(entries[0].extension.is_none());
                assert_eq!(entries[1].name, "file.txt");
                assert_eq!(entries[1].extension.as_deref(), Some("txt"));
                assert_eq!(common_parent_display.as_deref(), Some("/proj"));
            }
            other => panic!("expected FileList body, got {other:?}"),
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
        let dto = EntryPreviewDto::from_entry(&entry, None);
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
        let dto = EntryPreviewDto::from_entry(&entry, None);
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
}

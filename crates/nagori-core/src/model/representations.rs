use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{ClipboardSequence, SourceApp};

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

/// One validated, allowlisted representation extracted from a
/// `ClipboardSnapshot`.
///
/// Captures everything the storage layer needs to persist a row into
/// `entry_representations` (role + ordinal + mime + payload) while keeping
/// the IPC / DTO surface untouched — the value lives only in memory between
/// [`crate::EntryFactory`] and the insert path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredClipboardRepresentation {
    pub role: RepresentationRole,
    pub mime_type: String,
    pub ordinal: u32,
    pub data: RepresentationDataRef,
}

impl StoredClipboardRepresentation {
    #[must_use]
    pub fn byte_count(&self) -> usize {
        match &self.data {
            RepresentationDataRef::InlineText(text) => text.len(),
            RepresentationDataRef::DatabaseBlob(bytes) => bytes.len(),
            RepresentationDataRef::FilePaths(paths) => {
                // Stored as a JSON array in `text_content`; size the row by
                // the encoded byte length so the retention budget matches
                // what physically lands in SQLite.
                encode_file_paths(paths).len()
            }
        }
    }
}

/// Lightweight projection of `entry_representations` for IPC and palette
/// rendering.
///
/// Carries only the columns the frontend needs to render "HTML + Plain"
/// badges and the "preserved formats" row — no payload bytes — so the
/// search-list hot path can batch-fetch every result row without
/// decoding blobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepresentationSummary {
    pub role: RepresentationRole,
    pub mime_type: String,
    pub byte_count: u64,
}

impl RepresentationSummary {
    #[must_use]
    pub fn from_stored(rep: &StoredClipboardRepresentation) -> Self {
        Self {
            role: rep.role,
            mime_type: rep.mime_type.clone(),
            byte_count: rep.byte_count() as u64,
        }
    }
}

/// Downscaled image bytes plus dimensions, persisted in the
/// `entry_thumbnails` table.
///
/// Used by the desktop preview pane to keep the `WebView` from rendering
/// multi-MB originals every time the user navigates between rows. Lives
/// outside `StoredClipboardRepresentation` on purpose: copy-back
/// (`PasteFormat::Preserve`) walks every stored representation when the
/// user re-pastes an entry, and a downscaled PNG must never be promoted
/// to the system clipboard alongside (or in place of) the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailRecord {
    pub payload: Vec<u8>,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
}

/// Role of a stored representation inside an entry's representation set.
///
/// Maps 1:1 to the `entry_representations.role` SQL column. The variant
/// order also encodes the persisted ordinal ranking: primary < plain
/// fallback < alternatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepresentationRole {
    Primary,
    PlainFallback,
    Alternative,
}

impl RepresentationRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::PlainFallback => "plain_fallback",
            Self::Alternative => "alternative",
        }
    }

    /// Parse the SQL `entry_representations.role` string back into the
    /// runtime enum. Returns `None` for any other value so callers can
    /// surface a clear storage error instead of silently dropping a row.
    #[must_use]
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "primary" => Some(Self::Primary),
            "plain_fallback" => Some(Self::PlainFallback),
            "alternative" => Some(Self::Alternative),
            _ => None,
        }
    }
}

/// Payload kept on a [`StoredClipboardRepresentation`].
///
/// Text-shaped reps (plain, html, rtf) land in
/// `entry_representations.text_content`; image bytes land in
/// `entry_representations.payload_blob`; file URL lists are encoded into
/// `text_content` as a JSON array (see [`encode_file_paths`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepresentationDataRef {
    InlineText(String),
    DatabaseBlob(Vec<u8>),
    FilePaths(Vec<String>),
}

/// Encode a `FilePaths` list into the canonical `text_content` form: a JSON
/// array of strings.
///
/// The previous newline-joined encoding was lossy — a path containing a
/// newline (legal on Unix) split into multiple bogus entries on read. JSON
/// escapes such bytes, so the list round-trips exactly. Both the storage
/// insert and [`StoredClipboardRepresentation::byte_count`] go through here so
/// the retention accounting matches the bytes actually written.
#[must_use]
pub fn encode_file_paths(paths: &[String]) -> String {
    // Serialising a `Vec<String>` to JSON cannot fail (strings are always
    // valid JSON values), so the only error variant is unreachable.
    serde_json::to_string(paths).expect("Vec<String> always serialises to JSON")
}

/// Decode the `text_content` of a `text/uri-list` representation back into a
/// `FilePaths` list.
///
/// New rows are JSON arrays produced by [`encode_file_paths`]. Rows written by
/// older builds are newline-joined, which is not valid JSON for a realistic
/// path, so a parse failure falls back to the legacy newline split — keeping
/// existing histories readable after upgrade.
#[must_use]
pub fn decode_file_paths(stored: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(stored)
        .unwrap_or_else(|_| stored.split('\n').map(ToOwned::to_owned).collect())
}

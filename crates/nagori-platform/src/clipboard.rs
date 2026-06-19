use std::fmt::Display;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardData, ClipboardEntry, ClipboardRepresentation, ClipboardSequence,
    ClipboardSnapshot, ContentHash, ReadBudget, RepresentationDataRef, Result,
    StoredClipboardRepresentation,
};
use time::OffsetDateTime;

/// Maximum number of torn-read retries a polling clipboard adapter makes when
/// the change-count moves between the pre-read probe and the byte read.
///
/// macOS and Windows both re-probe up to this many times before accepting the
/// most recent read; sharing the bound here keeps the two adapters from
/// drifting apart.
pub const SNAPSHOT_CAPTURE_MAX_RETRIES: usize = 3;

/// A clipboard owner's explicit "do not record this in history" marker.
///
/// Producers that publish secrets or throwaway values onto the clipboard
/// flag them with a well-known marker type so cooperating clipboard
/// managers skip recording them. On macOS these are the nspasteboard.org
/// convention types `org.nspasteboard.ConcealedType` (passwords and other
/// secrets, set by password managers) and `org.nspasteboard.TransientType`
/// (content not meant to outlive the current paste). The kind is named
/// platform-neutrally rather than `PasteboardMarker` because the same
/// "owner-declared exclusion" contract has analogues every desktop adapter
/// now honours: the Windows adapter maps the `Clipboard Viewer Ignore` and
/// `ExcludeClipboardContentFromMonitorProcessing` formats, and the Linux
/// (Wayland) adapter maps KDE's `x-kde-passwordManagerHint` offer, onto this
/// same skip path — each meaning "refuse third-party history storage". Both
/// non-macOS conventions are presence-only secret markers with no transient
/// analogue, so they surface as [`Self::Concealed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardExclusionKind {
    /// Concealed secret (e.g. a password). The strongest signal — when an
    /// owner publishes both markers this one wins.
    Concealed,
    /// Transient content the owner does not want persisted.
    Transient,
}

/// Outcome of [`ClipboardReader::current_snapshot_with_max`].
///
/// `Oversized` carries the change-count `sequence` so the capture loop can
/// still anchor `last_sequence` and avoid re-reading the same oversized clip
/// every poll, plus `observed_bytes` / `limit` for the overflowing content
/// kind for audit logging. The variant is intentionally separate from
/// [`AppError`] because exceeding the configured per-kind budget (text or
/// image, see [`ReadBudget`]) is a benign skip, not a platform-level failure.
///
/// `Excluded` is the same shape for an owner-declared exclusion marker (see
/// [`ClipboardExclusionKind`]): the adapter detects the marker and skips the
/// clip without emitting its body, normally without reading it at all. It
/// carries the `sequence` so the capture loop anchors dedup and skips the
/// clip without ever materialising it as an entry.
#[derive(Debug)]
pub enum CapturedSnapshot {
    Captured(ClipboardSnapshot),
    Oversized {
        sequence: ClipboardSequence,
        observed_bytes: usize,
        limit: usize,
    },
    Excluded {
        sequence: ClipboardSequence,
        kind: ClipboardExclusionKind,
    },
}

#[async_trait]
pub trait ClipboardReader: Send + Sync {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot>;
    async fn current_sequence(&self) -> Result<ClipboardSequence>;

    /// Like [`Self::current_sequence`], but allows platforms that must read
    /// clipboard bytes to use the same per-kind pre-read budget as
    /// [`Self::current_snapshot_with_max`].
    async fn current_sequence_with_max(&self, _budget: ReadBudget) -> Result<ClipboardSequence> {
        self.current_sequence().await
    }

    /// Like [`Self::current_snapshot`] but rejects payloads that exceed the
    /// per-content-kind [`ReadBudget`] *before* materialising them into a Rust
    /// `Vec<u8>` / `String` whenever the platform exposes a cheap byte-length
    /// probe — image bytes against `budget.image_bytes`, everything else
    /// against `budget.text_bytes`.
    ///
    /// The default implementation falls back to the unbounded snapshot and
    /// inspects sizes after the fact; platform impls should override it to
    /// avoid loading huge clipboards into the daemon's address space at all.
    async fn current_snapshot_with_max(&self, budget: ReadBudget) -> Result<CapturedSnapshot> {
        let snapshot = self.current_snapshot().await?;
        if let Some((observed, limit)) = oversized_kind(&snapshot, budget) {
            Ok(CapturedSnapshot::Oversized {
                sequence: snapshot.sequence,
                observed_bytes: observed,
                limit,
            })
        } else {
            Ok(CapturedSnapshot::Captured(snapshot))
        }
    }
}

/// Per-kind byte total of a snapshot representation.
fn snapshot_rep_bytes(rep: &ClipboardRepresentation) -> usize {
    match &rep.data {
        ClipboardData::Text(text) => text.len(),
        ClipboardData::Bytes(bytes) => bytes.len(),
        ClipboardData::FilePaths(paths) => paths.iter().map(String::len).sum(),
    }
}

/// Whether a snapshot representation carries image bytes (mime `image/*` or a
/// raw byte payload), so it answers to the image budget rather than the text
/// budget.
fn is_image_snapshot_rep(rep: &ClipboardRepresentation) -> bool {
    rep.mime_type.starts_with("image/") || matches!(rep.data, ClipboardData::Bytes(_))
}

/// Report the first representation that overflows its content kind's budget,
/// as `(observed_bytes, limit)`.
///
/// Each representation is sized individually — image bytes against
/// `budget.image_bytes`, text / file-list bytes against `budget.text_bytes`.
/// Keeping the two budgets separate is what lets a screenshot survive a text
/// budget far smaller than its encoded size; sizing per representation (rather
/// than per-kind sum) leaves aggregate trimming to the capture loop's
/// `trim_alternatives_to_budget`, matching the per-platform adapters.
fn oversized_kind(snapshot: &ClipboardSnapshot, budget: ReadBudget) -> Option<(usize, usize)> {
    snapshot.representations.iter().find_map(|rep| {
        let bytes = snapshot_rep_bytes(rep);
        let limit = if is_image_snapshot_rep(rep) {
            budget.image_bytes
        } else {
            budget.text_bytes
        };
        (bytes > limit).then_some((bytes, limit))
    })
}

#[async_trait]
pub trait ClipboardWriter: Send + Sync {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()>;
    async fn write_plain(&self, entry: &ClipboardEntry) -> Result<()>;
    async fn write_text(&self, text: &str) -> Result<()>;

    /// Publish every stored representation for `entry` in a single
    /// pasteboard transaction.
    ///
    /// Used by `PasteFormat::Preserve` copy-back so a receiver that
    /// understands HTML / RTF / image bytes can pick the richest
    /// representation the source originally offered, while a plain-text
    /// target still finds the matching `text/plain` fallback. All three
    /// desktop adapters (macOS / Windows / Linux Wayland) override this
    /// and report `clipboard_multi_representation_write = Available`; the
    /// default implementation here delegates back to `write_entry` and
    /// exists for any future adapter that cannot publish a
    /// multi-representation transaction, preserving the primary-only
    /// contract.
    async fn write_representations(
        &self,
        entry: &ClipboardEntry,
        representations: &[StoredClipboardRepresentation],
    ) -> Result<()> {
        let _ = representations;
        self.write_entry(entry).await
    }

    /// Publish exactly one stored representation, with no fallback to the
    /// entry's primary content.
    ///
    /// Backs the "paste as <format>" picker: the runtime resolves the
    /// chosen MIME to a single representation and asks the adapter to put
    /// only that on the clipboard. Unlike [`Self::write_representations`]
    /// (Preserve copy-back, which falls back to `write_entry` when nothing
    /// is publishable), this must never silently substitute a different
    /// representation — the user picked a specific format, so a request the
    /// adapter cannot honour is an error rather than a surprise paste of
    /// the primary. The default impl refuses; adapters that advertise
    /// `clipboard_multi_representation_write` override it.
    async fn write_representation_exact(
        &self,
        representation: &StoredClipboardRepresentation,
    ) -> Result<()> {
        let _ = representation;
        Err(AppError::Unsupported(
            "this clipboard adapter cannot publish a single representation".to_owned(),
        ))
    }
}

#[async_trait]
impl<T: ClipboardReader + ?Sized> ClipboardReader for Arc<T> {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        (**self).current_snapshot().await
    }

    async fn current_sequence(&self) -> Result<ClipboardSequence> {
        (**self).current_sequence().await
    }

    async fn current_sequence_with_max(&self, budget: ReadBudget) -> Result<ClipboardSequence> {
        (**self).current_sequence_with_max(budget).await
    }

    async fn current_snapshot_with_max(&self, budget: ReadBudget) -> Result<CapturedSnapshot> {
        (**self).current_snapshot_with_max(budget).await
    }
}

#[async_trait]
impl<T: ClipboardWriter + ?Sized> ClipboardWriter for Arc<T> {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
        (**self).write_entry(entry).await
    }

    async fn write_plain(&self, entry: &ClipboardEntry) -> Result<()> {
        (**self).write_plain(entry).await
    }

    async fn write_text(&self, text: &str) -> Result<()> {
        (**self).write_text(text).await
    }

    async fn write_representations(
        &self,
        entry: &ClipboardEntry,
        representations: &[StoredClipboardRepresentation],
    ) -> Result<()> {
        (**self).write_representations(entry, representations).await
    }

    async fn write_representation_exact(
        &self,
        representation: &StoredClipboardRepresentation,
    ) -> Result<()> {
        (**self).write_representation_exact(representation).await
    }
}

/// True when at least one stored rep has a mapping in the platform
/// publishers' shared MIME table.
///
/// Pre-scan used by every adapter's `write_representations` so an entry
/// whose stored reps are *all* outside the publisher's table (e.g. only
/// `application/json` without a plain fallback) falls back through
/// `write_entry` instead of clearing the OS clipboard and erroring after the
/// fact. `write_representation_exact` uses the same scan to refuse a MIME it
/// cannot publish without touching the clipboard.
///
/// All three desktop adapters publish the same table — plain text / HTML /
/// RTF as inline text, the allowlisted image formats (PNG / TIFF / JPEG /
/// GIF / WebP) as blobs, and non-empty file lists — so the scan is shared
/// here. If one platform's table ever diverges, parameterise the MIME sets
/// instead of forking the function back into the adapters.
#[must_use]
pub fn has_publishable_representation(reps: &[StoredClipboardRepresentation]) -> bool {
    reps.iter()
        .any(|rep| match (rep.mime_type.as_str(), &rep.data) {
            (
                "text/plain" | "text/html" | "application/rtf",
                RepresentationDataRef::InlineText(_),
            )
            | (
                "image/png" | "image/tiff" | "image/jpeg" | "image/gif" | "image/webp",
                RepresentationDataRef::DatabaseBlob(_),
            ) => true,
            ("text/uri-list", RepresentationDataRef::FilePaths(paths)) => !paths.is_empty(),
            _ => false,
        })
}

#[derive(Debug, Default)]
pub struct MemoryClipboard {
    state: Mutex<Option<String>>,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_text(&self) -> Option<String> {
        self.state.lock().ok().and_then(|guard| guard.clone())
    }
}

#[async_trait]
impl ClipboardReader for MemoryClipboard {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        let text = self
            .state
            .lock()
            .map_err(|err| lock_err(&err))?
            .clone()
            .unwrap_or_default();
        let sequence = ClipboardSequence::content_hash(ContentHash::sha256(text.as_bytes()).value);
        Ok(ClipboardSnapshot {
            sequence,
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text(text),
            }],
        })
    }

    async fn current_sequence(&self) -> Result<ClipboardSequence> {
        let text = self
            .state
            .lock()
            .map_err(|err| lock_err(&err))?
            .clone()
            .unwrap_or_default();
        Ok(ClipboardSequence::content_hash(
            ContentHash::sha256(text.as_bytes()).value,
        ))
    }
}

#[async_trait]
impl ClipboardWriter for MemoryClipboard {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()> {
        let text = entry
            .plain_text()
            .ok_or_else(|| AppError::Unsupported("non-text clipboard entry".to_owned()))?;
        self.write_text(text).await
    }

    async fn write_plain(&self, entry: &ClipboardEntry) -> Result<()> {
        self.write_entry(entry).await
    }

    async fn write_text(&self, text: &str) -> Result<()> {
        *self.state.lock().map_err(|err| lock_err(&err))? = Some(text.to_owned());
        Ok(())
    }

    async fn write_representation_exact(
        &self,
        representation: &StoredClipboardRepresentation,
    ) -> Result<()> {
        // The in-memory adapter is text-only, so it can honour an exact
        // paste of an inline-text representation but nothing binary; a
        // non-text request is refused rather than silently dropped.
        match &representation.data {
            RepresentationDataRef::InlineText(text) => self.write_text(text).await,
            _ => Err(AppError::Unsupported(
                "memory clipboard only stores text representations".to_owned(),
            )),
        }
    }
}

/// Wrap any displayable backend error as an [`AppError::Platform`].
///
/// Clipboard adapters call this for `arboard` failures. Keeping it generic
/// over [`Display`] lets `nagori-platform` own the helper without taking a
/// dependency on a specific clipboard backend crate.
#[must_use]
pub fn platform_err<E: Display + ?Sized>(err: &E) -> AppError {
    AppError::Platform(err.to_string())
}

/// Wrap a poisoned-lock error as an [`AppError::Platform`].
///
/// A distinct name from [`platform_err`] so the call site reads as "the mutex
/// guarding the clipboard was poisoned" rather than a generic backend failure.
#[must_use]
pub fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::Platform(err.to_string())
}

#[cfg(test)]
mod tests {
    use nagori_core::{
        EntryFactory, RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation,
    };

    use super::*;

    #[tokio::test]
    async fn write_representations_default_falls_back_to_write_entry() {
        // Adapters without `clipboard_multi_representation_write` keep the
        // default impl, which has to publish the entry's primary text
        // through `write_entry`. `MemoryClipboard` inherits that path —
        // exercising it locks the contract that the daemon's Preserve
        // copy-back stays functional on any adapter (test stub or future
        // host) that hasn't opted into multi-rep yet.
        let clipboard = MemoryClipboard::new();
        let entry = EntryFactory::from_text("primary body");
        let reps = vec![
            StoredClipboardRepresentation {
                role: RepresentationRole::Primary,
                mime_type: "text/html".to_owned(),
                ordinal: 0,
                data: RepresentationDataRef::InlineText("<p>primary body</p>".to_owned()),
            },
            StoredClipboardRepresentation {
                role: RepresentationRole::PlainFallback,
                mime_type: "text/plain".to_owned(),
                ordinal: 1,
                data: RepresentationDataRef::InlineText("primary body".to_owned()),
            },
        ];

        clipboard
            .write_representations(&entry, &reps)
            .await
            .expect("default fallback must succeed for text entries");
        assert_eq!(clipboard.current_text().as_deref(), Some("primary body"));
    }

    #[tokio::test]
    async fn write_representation_exact_publishes_text_and_refuses_binary() {
        // The memory adapter honours an inline-text exact paste verbatim
        // (the selected rep's body, not the entry's primary)...
        let clipboard = MemoryClipboard::new();
        let html = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/html".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("<p>just me</p>".to_owned()),
        };
        clipboard
            .write_representation_exact(&html)
            .await
            .expect("inline-text exact paste must succeed");
        assert_eq!(clipboard.current_text().as_deref(), Some("<p>just me</p>"));

        // ...and refuses a binary rep rather than falling back to anything.
        let image = StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "image/png".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::DatabaseBlob(vec![0x89, 0x50]),
        };
        assert!(clipboard.write_representation_exact(&image).await.is_err());
        // The refused write left the prior contents untouched.
        assert_eq!(clipboard.current_text().as_deref(), Some("<p>just me</p>"));
    }

    #[test]
    fn has_publishable_representation_matches_known_mimes() {
        let plain = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("hi".to_owned()),
        };
        let html = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/html".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::InlineText("<p>hi</p>".to_owned()),
        };
        let png = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "image/png".to_owned(),
            ordinal: 2,
            data: RepresentationDataRef::DatabaseBlob(vec![0x89, 0x50, 0x4e, 0x47]),
        };
        let paths = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 3,
            data: RepresentationDataRef::FilePaths(vec!["/tmp/one".to_owned()]),
        };
        assert!(has_publishable_representation(&[plain]));
        assert!(has_publishable_representation(&[html]));
        assert!(has_publishable_representation(&[png]));
        assert!(has_publishable_representation(&[paths]));
    }

    #[test]
    fn has_publishable_representation_rejects_unmapped_mimes() {
        let json = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "application/json".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("{}".to_owned()),
        };
        let empty_paths = StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/uri-list".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::FilePaths(Vec::new()),
        };
        assert!(!has_publishable_representation(&[]));
        assert!(!has_publishable_representation(&[json, empty_paths]));
    }
}

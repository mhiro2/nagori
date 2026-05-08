use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardData, ClipboardEntry, ClipboardRepresentation, ClipboardSequence,
    ClipboardSnapshot, ContentHash, Result,
};
use time::OffsetDateTime;

/// Outcome of [`ClipboardReader::current_snapshot_with_max`].
///
/// `Oversized` carries the change-count `sequence` so the capture loop can
/// still anchor `last_sequence` and avoid re-reading the same oversized clip
/// every poll, plus `observed_bytes` for audit logging. The variant is
/// intentionally separate from [`AppError`] because hitting the configured
/// `max_entry_size_bytes` is a benign skip, not a platform-level failure.
#[derive(Debug)]
pub enum CapturedSnapshot {
    Captured(ClipboardSnapshot),
    Oversized {
        sequence: ClipboardSequence,
        observed_bytes: usize,
        limit: usize,
    },
}

#[async_trait]
pub trait ClipboardReader: Send + Sync {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot>;
    async fn current_sequence(&self) -> Result<ClipboardSequence>;

    /// Like [`Self::current_snapshot`] but rejects payloads larger than
    /// `max_bytes` *before* materialising them into a Rust `Vec<u8>` /
    /// `String` whenever the platform exposes a cheap byte-length probe.
    ///
    /// The default implementation falls back to the unbounded snapshot and
    /// inspects sizes after the fact; platform impls should override it to
    /// avoid loading huge clipboards into the daemon's address space at all.
    async fn current_snapshot_with_max(&self, max_bytes: usize) -> Result<CapturedSnapshot> {
        let snapshot = self.current_snapshot().await?;
        let observed = total_payload_bytes(&snapshot);
        if observed > max_bytes {
            Ok(CapturedSnapshot::Oversized {
                sequence: snapshot.sequence,
                observed_bytes: observed,
                limit: max_bytes,
            })
        } else {
            Ok(CapturedSnapshot::Captured(snapshot))
        }
    }
}

fn total_payload_bytes(snapshot: &ClipboardSnapshot) -> usize {
    snapshot
        .representations
        .iter()
        .map(|rep| match &rep.data {
            ClipboardData::Text(text) => text.len(),
            ClipboardData::Bytes(bytes) => bytes.len(),
            ClipboardData::FilePaths(paths) => paths.iter().map(String::len).sum(),
        })
        .sum()
}

#[async_trait]
pub trait ClipboardWriter: Send + Sync {
    async fn write_entry(&self, entry: &ClipboardEntry) -> Result<()>;
    async fn write_plain(&self, entry: &ClipboardEntry) -> Result<()>;
    async fn write_text(&self, text: &str) -> Result<()>;
}

#[async_trait]
impl<T: ClipboardReader + ?Sized> ClipboardReader for Arc<T> {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot> {
        (**self).current_snapshot().await
    }

    async fn current_sequence(&self) -> Result<ClipboardSequence> {
        (**self).current_sequence().await
    }

    async fn current_snapshot_with_max(&self, max_bytes: usize) -> Result<CapturedSnapshot> {
        (**self).current_snapshot_with_max(max_bytes).await
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
        let sequence = ClipboardSequence(ContentHash::sha256(text.as_bytes()).value);
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
        Ok(ClipboardSequence(
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
}

fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::Platform(err.to_string())
}

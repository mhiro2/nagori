use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nagori_core::{
    AppError, ClipboardData, ClipboardEntry, ClipboardRepresentation, ClipboardSequence,
    ClipboardSnapshot, ContentHash, Result,
};
use time::OffsetDateTime;

#[async_trait]
pub trait ClipboardReader: Send + Sync {
    async fn current_snapshot(&self) -> Result<ClipboardSnapshot>;
    async fn current_sequence(&self) -> Result<ClipboardSequence>;
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

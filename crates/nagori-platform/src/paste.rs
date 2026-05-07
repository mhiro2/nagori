use async_trait::async_trait;
use nagori_core::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteResult {
    pub pasted: bool,
    pub message: Option<String>,
}

#[async_trait]
pub trait PasteController: Send + Sync {
    async fn paste_frontmost(&self) -> Result<PasteResult>;
}

#[derive(Debug, Default)]
pub struct NoopPasteController;

#[async_trait]
impl PasteController for NoopPasteController {
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        Ok(PasteResult {
            pasted: false,
            message: Some("auto-paste is not enabled on this platform".to_owned()),
        })
    }
}

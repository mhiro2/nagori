use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::Result;

/// Where a recognized line sits in its image, as fractions of the image's
/// width and height with the origin at the top-left corner, so the box can
/// be drawn over the image at any display size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One line of text recognized in an image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecognizedLine {
    pub text: String,
    pub bounds: TextBounds,
}

/// Text recognized in an image, line by line in reading order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecognizedText {
    pub lines: Vec<RecognizedLine>,
}

impl RecognizedText {
    /// The recognized lines joined with newlines: the form the text is
    /// copied, searched and classified in.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// On-device text recognition for images (macOS Vision today).
///
/// The daemon drives it for both the explicit "extract text" action and the
/// opt-in background indexer that makes screenshots searchable by the text
/// in them. Implementations must stay on the device.
#[async_trait]
pub trait TextRecognizer: Send + Sync {
    /// Recognize the text in one encoded image (PNG / JPEG / TIFF / …).
    ///
    /// # Errors
    ///
    /// When the image cannot be decoded or the recognizer fails.
    async fn recognize(&self, image: Bytes) -> Result<RecognizedText>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_joins_lines_in_reading_order() {
        let bounds = TextBounds {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 0.1,
        };
        let recognized = RecognizedText {
            lines: vec![
                RecognizedLine {
                    text: "error: connection refused".to_owned(),
                    bounds,
                },
                RecognizedLine {
                    text: "接続できません".to_owned(),
                    bounds,
                },
            ],
        };
        assert_eq!(
            recognized.text(),
            "error: connection refused\n接続できません"
        );
        assert!(RecognizedText::default().is_empty());
    }
}

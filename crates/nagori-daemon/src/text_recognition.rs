//! On-device text recognition in image entries.
//!
//! Two paths share one privacy shaping step ([`shape_recognized_text`]):
//!
//! - the explicit "extract text" action ([`NagoriRuntime::recognize_entry_text`]),
//!   which reads one image on request, and
//! - the opt-in background indexer ([`NagoriRuntime::run_text_recognition_indexer`]),
//!   which reads every Public / Unknown image once so screenshots are found by
//!   the text they show.
//!
//! The recognized text runs through the same classifier as a captured clip.
//! Text that reads as private or secret is redacted before it is stored or
//! shown, and because the image itself still shows that content, the entry's
//! sensitivity is raised with it — whether or not the text is stored — so the
//! thumbnail and preview stop showing the picture and deletion erases it at
//! once. Masked images are never read. Stored text is tied to the content
//! policy it was shaped under (`AppSettings::semantic_policy_hash`), so a
//! policy edit takes it out of the index and the image is read again.

use std::time::Duration;

use nagori_core::{
    AppError, AppSettings, ClipboardContent, EntryFactory, EntryId, EntryRepository,
    RecognizedLine, RecognizedText, Result, Sensitivity, SensitivityClassifier, TextRecognizer,
};
use nagori_platform::NO_TEXT_RECOGNIZER_REASON;
use nagori_storage::TextRecognitionOutcome;

use crate::runtime::{NagoriRuntime, ShutdownHandle};

/// Images read per pending-list fetch. Small so a settings change or shutdown
/// is observed between batches.
const RECOGNITION_BATCH: usize = 8;

/// Pause between images in a backfill, so a long history is read in the
/// background without pinning a core.
const RECOGNITION_SPACING: Duration = Duration::from_millis(250);

/// Upper bound on one recognition. The first request also loads the model,
/// which can take several seconds; a recognizer still busy past this is
/// treated as unable to read the image so the backfill moves on.
const RECOGNITION_TIMEOUT: Duration = Duration::from_mins(2);

/// Idle re-check cadence, covering any missed wake.
const IDLE_TICK: Duration = Duration::from_mins(1);

/// Text recognized in an image, shaped for storage and display.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedText {
    pub text: RecognizedText,
    /// The image itself was masked because its text read as private or
    /// secret; views showing it should refresh.
    pub masked: bool,
}

/// Redact recognized text that classifies as private or secret, and report
/// the sensitivity the image itself should carry.
///
/// The whole text is classified and redacted at once, so a secret spanning
/// lines (a PEM block, a user pattern across a line break) is caught as a
/// whole; the redacted text is then split back over the recognized lines in
/// order. A redaction that swallows line breaks leaves fewer lines than were
/// recognized, and the lines it consumed are dropped rather than shown. A
/// `Blocked` classification (a capture-time drop) maps to `Secret`: the image
/// is already stored, so the protective choice is to mask it rather than
/// hide it from the history.
#[must_use]
pub fn shape_recognized_text(
    recognized: RecognizedText,
    classifier: &SensitivityClassifier,
) -> (RecognizedText, Option<Sensitivity>) {
    if recognized.is_empty() {
        return (recognized, None);
    }
    let text = recognized.text();
    let raised = match classifier
        .classify(&EntryFactory::from_text(text.as_str()))
        .sensitivity
    {
        Sensitivity::Public | Sensitivity::Unknown => return (recognized, None),
        Sensitivity::Blocked => Sensitivity::Secret,
        other => other,
    };
    let redacted = classifier.redact(&text);
    let lines = recognized
        .lines
        .into_iter()
        .zip(redacted.split('\n'))
        .map(|(line, text)| RecognizedLine {
            text: text.to_owned(),
            ..line
        })
        .filter(|line| !line.text.trim().is_empty())
        .collect();
    (RecognizedText { lines }, Some(raised))
}

impl NagoriRuntime {
    /// Wake the background indexer after a capture so a new screenshot is
    /// read promptly. A no-op beyond signalling.
    pub fn notify_text_recognition_capture(&self) {
        self.text_recognition_wake.notify_one();
    }

    /// Recognize the text in one image entry on request, for the "extract
    /// text" action.
    ///
    /// Returns the stored recognition when the image was already read under
    /// the current policy; otherwise reads it now. The text is privacy-shaped
    /// (see [`shape_recognized_text`]) and, when it shows the image holds
    /// private or secret content, the image is masked even with image text
    /// search off. The text is stored and indexed only while image text
    /// search is on.
    ///
    /// # Errors
    ///
    /// [`AppError::Unsupported`] without a recognizer on this host,
    /// [`AppError::NotFound`] for a missing entry, [`AppError::InvalidInput`]
    /// for a non-image or masked entry, and recognizer failures.
    pub async fn recognize_entry_text(&self, id: EntryId) -> Result<ExtractedText> {
        let recognizer = self
            .text_recognizer
            .clone()
            .ok_or_else(|| AppError::Unsupported(NO_TEXT_RECOGNIZER_REASON.to_owned()))?;
        let entry = self.store.get(id).await?.ok_or(AppError::NotFound)?;
        if !matches!(entry.content, ClipboardContent::Image(_)) {
            return Err(AppError::InvalidInput(
                "only images carry text to recognize".to_owned(),
            ));
        }
        if !matches!(
            entry.sensitivity,
            Sensitivity::Public | Sensitivity::Unknown
        ) {
            return Err(AppError::InvalidInput(
                "masked images are not read".to_owned(),
            ));
        }
        let settings = self.current_settings();
        let policy_hash = settings.semantic_policy_hash();
        if let Some(stored) = self
            .store
            .get_text_recognition(id, policy_hash.clone())
            .await?
        {
            return Ok(ExtractedText {
                text: stored,
                masked: false,
            });
        }
        let classifier = SensitivityClassifier::try_new(settings)?;
        let content_hash = entry.metadata.content_hash.value;
        let bytes = self
            .store
            .readable_image_payload(id, content_hash.clone())
            .await?
            .ok_or_else(|| AppError::InvalidInput("the image cannot be read".to_owned()))?;
        let read = recognizer.recognize(bytes.into()).await?;
        let (text, raise_to) = shape_recognized_text(read, &classifier);
        let masked = self
            .store_extracted_text(id, content_hash, &policy_hash, &text, raise_to)
            .await?;
        Ok(ExtractedText { text, masked })
    }

    /// Store an on-request recognition when image text search is on (and
    /// still under the policy it was shaped by), and mask the image whenever
    /// its text called for it. Returns whether the image was masked.
    async fn store_extracted_text(
        &self,
        id: EntryId,
        content_hash: String,
        policy_hash: &str,
        text: &RecognizedText,
        raise_to: Option<Sensitivity>,
    ) -> Result<bool> {
        let _gate = self.text_recognition_gate.read().await;
        let settings = self.current_settings();
        let store_text =
            settings.ai.image_text_index_enabled && settings.semantic_policy_hash() == policy_hash;
        let masked = if store_text {
            self.invalidate_search_cache();
            let recorded = self
                .store
                .record_text_recognition(
                    id,
                    content_hash,
                    policy_hash.to_owned(),
                    TextRecognitionOutcome::Recognized {
                        text: text.clone(),
                        raise_to,
                    },
                )
                .await?;
            self.invalidate_search_cache();
            recorded && raise_to.is_some()
        } else if let Some(raised) = raise_to {
            self.store
                .raise_image_sensitivity(id, content_hash, raised)
                .await?
        } else {
            false
        };
        if masked {
            self.invalidate_search_cache();
            self.notify_external_mutation();
        }
        Ok(masked)
    }

    /// Background image-text indexer.
    ///
    /// While `image_text_index_enabled` is on, first drops text shaped under
    /// an older content policy, then reads every Public / Unknown image that
    /// has no recognition for its current bytes and policy — new captures
    /// first — and indexes the text. While it is off, erases any recognized
    /// text from the index, so turning the setting off is also how a user
    /// takes it back. Wakes on a capture, a settings change, or the idle tick.
    pub async fn run_text_recognition_indexer(self, mut shutdown: ShutdownHandle) {
        let mut settings_rx = self.settings_subscribe();
        loop {
            let settings = settings_rx.borrow().clone();
            if settings.ai.image_text_index_enabled {
                if let Some(recognizer) = self.text_recognizer.clone() {
                    self.text_recognition_pass(recognizer.as_ref(), &settings, &shutdown)
                        .await;
                }
            } else {
                self.clear_text_recognition(None).await;
            }
            tokio::select! {
                () = shutdown.cancelled() => return,
                changed = settings_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                () = self.text_recognition_wake.notified() => {}
                () = tokio::time::sleep(IDLE_TICK) => {}
            }
        }
    }

    async fn clear_text_recognition(&self, keep_policy: Option<String>) {
        let _gate = self.text_recognition_gate.write().await;
        if keep_policy.is_none() && !self.store.has_text_recognition().await.unwrap_or(true) {
            return;
        }
        self.invalidate_search_cache();
        match self.store.clear_text_recognition(keep_policy).await {
            Ok(0) => {}
            Ok(cleared) => tracing::info!(cleared, "image_text_index_cleared"),
            Err(err) => tracing::warn!(error = %err, "image_text_index_clear_failed"),
        }
        self.invalidate_search_cache();
    }

    /// Whether the pass started under `settings` may still store results:
    /// the setting is still on and the content policy has not moved.
    fn text_recognition_still_current(&self, settings: &AppSettings) -> bool {
        let current = self.current_settings();
        current.ai.image_text_index_enabled
            && current.semantic_policy_hash() == settings.semantic_policy_hash()
    }

    async fn text_recognition_pass(
        &self,
        recognizer: &dyn TextRecognizer,
        settings: &AppSettings,
        shutdown: &ShutdownHandle,
    ) {
        let policy_hash = settings.semantic_policy_hash();
        // A broken `regex_denylist` fails closed (nothing is read) rather
        // than indexing text the user's rules would have redacted.
        let classifier = match SensitivityClassifier::try_new(settings.clone()) {
            Ok(classifier) => classifier,
            Err(err) => {
                tracing::warn!(error = %err, "image_text_classifier_build_failed");
                return;
            }
        };
        self.clear_text_recognition(Some(policy_hash.clone())).await;
        while !shutdown.is_cancelled() && self.text_recognition_still_current(settings) {
            let pending = match self
                .store
                .text_recognition_pending(RECOGNITION_BATCH, policy_hash.clone())
                .await
            {
                Ok(pending) if !pending.is_empty() => pending,
                Ok(_) => return,
                Err(err) => {
                    tracing::warn!(error = %err, "image_text_pending_failed");
                    return;
                }
            };
            for item in pending {
                if shutdown.is_cancelled() {
                    return;
                }
                let outcome = self
                    .recognize_for_index(recognizer, item.entry_id, &item.content_hash, &classifier)
                    .await;
                if !self
                    .record_indexed_text(settings, &policy_hash, item, outcome)
                    .await
                {
                    return;
                }
                tokio::time::sleep(RECOGNITION_SPACING).await;
            }
        }
    }

    /// Store one background recognition, re-checking under the gate that the
    /// pass's setting and policy still hold. Returns `false` when the pass
    /// should stop.
    async fn record_indexed_text(
        &self,
        settings: &AppSettings,
        policy_hash: &str,
        item: nagori_storage::PendingTextRecognition,
        outcome: TextRecognitionOutcome,
    ) -> bool {
        let _gate = self.text_recognition_gate.read().await;
        if !self.text_recognition_still_current(settings) {
            return false;
        }
        let masks = matches!(
            outcome,
            TextRecognitionOutcome::Recognized {
                raise_to: Some(_),
                ..
            }
        );
        self.invalidate_search_cache();
        let recorded = self
            .store
            .record_text_recognition(
                item.entry_id,
                item.content_hash,
                policy_hash.to_owned(),
                outcome,
            )
            .await;
        self.invalidate_search_cache();
        match recorded {
            Ok(recorded) => {
                if recorded && masks {
                    self.notify_external_mutation();
                }
                true
            }
            Err(err) => {
                tracing::warn!(error = %err, "image_text_record_failed");
                false
            }
        }
    }

    async fn recognize_for_index(
        &self,
        recognizer: &dyn TextRecognizer,
        id: EntryId,
        content_hash: &str,
        classifier: &SensitivityClassifier,
    ) -> TextRecognitionOutcome {
        let bytes = match self
            .store
            .readable_image_payload(id, content_hash.to_owned())
            .await
        {
            Ok(Some(bytes)) => bytes,
            // Masked, changed or gone since it was listed: recording is
            // refused for the same reasons, so nothing is stored.
            Ok(None) => return TextRecognitionOutcome::Failed,
            Err(err) => {
                tracing::warn!(error = %err, "image_text_payload_failed");
                return TextRecognitionOutcome::Failed;
            }
        };
        match tokio::time::timeout(RECOGNITION_TIMEOUT, recognizer.recognize(bytes.into())).await {
            Ok(Ok(read)) => {
                let (text, raise_to) = shape_recognized_text(read, classifier);
                TextRecognitionOutcome::Recognized { text, raise_to }
            }
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "image_text_unreadable");
                TextRecognitionOutcome::Failed
            }
            Err(_) => {
                tracing::warn!("image_text_recognition_timed_out");
                TextRecognitionOutcome::Failed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use nagori_core::TextBounds;

    use super::*;

    fn lines(texts: &[&str]) -> RecognizedText {
        RecognizedText {
            lines: texts
                .iter()
                .map(|text| RecognizedLine {
                    text: (*text).to_owned(),
                    bounds: TextBounds {
                        x: 0.0,
                        y: 0.0,
                        width: 1.0,
                        height: 0.1,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn a_secret_spanning_lines_is_redacted_as_a_whole() {
        let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
        let recognized = lines(&[
            "deploy key:",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW",
            "QyNTUxOQAAACDx5k2M6pnF7z2pPZy0bIr0aJrXOL0R2uLRA0k6t8aZzgAAAJgcmF3dGV4",
            "-----END OPENSSH PRIVATE KEY-----",
        ]);
        let (shaped, raised) = shape_recognized_text(recognized, &classifier);
        assert_eq!(raised, Some(Sensitivity::Secret));
        let text = shaped.text();
        assert!(text.contains("deploy key:"), "{text}");
        assert!(!text.contains("b3BlbnNz"), "{text}");
        assert!(!text.contains("QyNTUxOQ"), "{text}");
    }

    #[test]
    fn public_text_passes_through_untouched() {
        let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
        let recognized = lines(&["error: connection refused"]);
        let (shaped, raised) = shape_recognized_text(recognized.clone(), &classifier);
        assert_eq!(raised, None);
        assert_eq!(shaped, recognized);
    }
}

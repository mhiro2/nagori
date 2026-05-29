//! The Apple Foundation Models text-generation backend.
//!
//! Implements `nagori-ai`'s [`TextGenerator`] over the Swift bridge so the
//! daemon can inject it into an `AiEngine` without taking any Apple dependency
//! itself. Only [`AiActionId::Summarize`] is wired today; the other
//! text-generation actions return a capability mismatch until their prompts and
//! gating land.

use async_trait::async_trait;
use nagori_ai::{
    AiEventStream, BackendAvailability, BackendUnavailableReason, TextGenerationCapabilities,
    TextGenerationRequest, TextGenerator,
};
use nagori_core::{AiActionId, AiError, AiErrorCode};
use tokio_util::sync::CancellationToken;

use crate::availability::AppleAvailability;
use crate::bridge;

/// Text generation backed by `SystemLanguageModel` via the Swift bridge.
#[derive(Debug, Clone, Default)]
pub struct AppleFoundationBackend;

impl AppleFoundationBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TextGenerator for AppleFoundationBackend {
    fn capabilities(&self) -> TextGenerationCapabilities {
        TextGenerationCapabilities {
            streaming: true,
            // Guided generation lands with the structured-extraction action.
            guided_generation: false,
            on_device: true,
        }
    }

    async fn availability(&self) -> BackendAvailability {
        map_availability(bridge::probe_real_availability())
    }

    async fn stream_text(
        &self,
        req: TextGenerationRequest,
        cancel: CancellationToken,
    ) -> Result<AiEventStream, AiError> {
        match req.action {
            AiActionId::Summarize => {
                // Re-probe so a direct caller (not just the engine) still gets a
                // clean synchronous error rather than a model-side failure.
                if let BackendAvailability::Unavailable(reason) =
                    map_availability(bridge::probe_real_availability())
                {
                    return Err(reason.into_error());
                }
                Ok(bridge::summarize_stream(&req.input, cancel))
            }
            other => Err(AiError::new(
                AiErrorCode::CapabilityMismatch,
                format!("{other:?} is not implemented by the Apple text generator"),
            )),
        }
    }
}

/// Maps the crate's [`AppleAvailability`] onto `nagori-ai`'s backend-level
/// availability.
const fn map_availability(availability: AppleAvailability) -> BackendAvailability {
    match availability {
        AppleAvailability::Available => BackendAvailability::Available,
        AppleAvailability::DeviceNotEligible => {
            BackendAvailability::Unavailable(BackendUnavailableReason::DeviceNotEligible)
        }
        AppleAvailability::AppleIntelligenceNotEnabled => {
            BackendAvailability::Unavailable(BackendUnavailableReason::NotEnabled)
        }
        AppleAvailability::ModelNotReady => {
            BackendAvailability::Unavailable(BackendUnavailableReason::ModelNotReady)
        }
        AppleAvailability::RateLimited => {
            BackendAvailability::Unavailable(BackendUnavailableReason::RateLimited)
        }
        AppleAvailability::Unknown => {
            BackendAvailability::Unavailable(BackendUnavailableReason::Unknown)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_advertise_streaming_on_device() {
        let caps = AppleFoundationBackend::new().capabilities();
        assert!(caps.streaming);
        assert!(caps.on_device);
        assert!(!caps.guided_generation);
    }

    /// End-to-end smoke test against the live on-device model. Skips when Apple
    /// Intelligence is unavailable (e.g. CI), so it never fails a headless run
    /// but exercises the full FFI → `LanguageModelSession` → stream path on a
    /// machine where the model is ready.
    #[tokio::test]
    async fn real_summarize_streams_to_done_when_available() {
        use futures::StreamExt;
        use nagori_core::{AiEvent, AiRequestOptions, RequestId};
        use tokio_util::sync::CancellationToken;

        let backend = AppleFoundationBackend::new();
        if backend.availability().await != BackendAvailability::Available {
            eprintln!("skipping real_summarize: Apple Intelligence unavailable");
            return;
        }

        let req = TextGenerationRequest {
            request_id: RequestId::new(),
            action: AiActionId::Summarize,
            input: "Nagori is a local-first clipboard history manager. It captures \
                    clipboard entries, classifies their sensitivity, and lets users \
                    search and paste them quickly from a palette."
                .to_owned(),
            options: AiRequestOptions::default(),
            guided_schema: None,
        };
        let mut stream = backend
            .stream_text(req, CancellationToken::new())
            .await
            .expect("summarize should start when available");

        let mut buf = String::new();
        let mut final_text = None;
        while let Some(item) = stream.next().await {
            match item.expect("no stream error") {
                AiEvent::Delta { text, .. } => buf.push_str(&text),
                AiEvent::Replace { text, .. } => buf = text,
                AiEvent::Done {
                    final_text: text, ..
                } => {
                    final_text = Some(text);
                    break;
                }
                AiEvent::Cancelled => panic!("unexpected cancel"),
            }
        }
        let final_text = final_text.expect("stream must finish with Done");
        assert!(!final_text.trim().is_empty(), "summary should be non-empty");
        assert_eq!(
            buf, final_text,
            "reconstructed deltas must match final text"
        );
    }

    #[test]
    fn availability_maps_every_apple_state() {
        assert_eq!(
            map_availability(AppleAvailability::Available),
            BackendAvailability::Available
        );
        assert_eq!(
            map_availability(AppleAvailability::AppleIntelligenceNotEnabled),
            BackendAvailability::Unavailable(BackendUnavailableReason::NotEnabled)
        );
        assert_eq!(
            map_availability(AppleAvailability::DeviceNotEligible),
            BackendAvailability::Unavailable(BackendUnavailableReason::DeviceNotEligible)
        );
        assert_eq!(
            map_availability(AppleAvailability::Unknown),
            BackendAvailability::Unavailable(BackendUnavailableReason::Unknown)
        );
    }
}

//! The Apple Translation framework backend.
//!
//! Implements `nagori-ai`'s [`Translator`] over the Swift bridge so the daemon
//! can inject it into an `AiEngine` without taking any Apple dependency itself.
//! Translation is one-shot (not streamed): the engine adapts the single
//! [`TranslationOutput`] into a terminal `Done` event.

use async_trait::async_trait;
use nagori_ai::{
    BackendAvailability, BackendUnavailableReason, TranslationOutput, TranslationRequest,
    Translator,
};
use nagori_core::AiError;
use tokio_util::sync::CancellationToken;

use crate::bridge;

/// Translation backed by `TranslationSession` via the Swift bridge.
#[derive(Debug, Clone, Default)]
pub struct AppleTranslateBackend;

impl AppleTranslateBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Translator for AppleTranslateBackend {
    async fn availability(&self) -> BackendAvailability {
        // The Translation framework ships with macOS (>= 15), so on the app's
        // minimum target (26.0) it is always present. Per-pair readiness —
        // including any language-pack download — is resolved by `pair_status`
        // and surfaced precisely at translate time, not here.
        BackendAvailability::Available
    }

    async fn pair_status(&self, source: Option<&str>, target: &str) -> BackendAvailability {
        // The Swift probe parks its calling thread on a `DispatchSemaphore`
        // for up to 3 s, so it runs on the blocking pool instead of wedging
        // a tokio worker. A join failure maps to the probe's own "unknown"
        // code rather than panicking an availability query.
        let source = source.map(str::to_owned);
        let target = target.to_owned();
        let code = tokio::task::spawn_blocking(move || {
            bridge::translation_pair_status(source.as_deref(), &target)
        })
        .await
        .unwrap_or(3);
        map_pair_status(code)
    }

    async fn translate(
        &self,
        req: TranslationRequest,
        // Cancellation is observed by the engine's stream wrapper (it drops this
        // future); the one-shot translate has no polling point of its own.
        _cancel: CancellationToken,
    ) -> Result<TranslationOutput, AiError> {
        bridge::translate(
            &req.input,
            req.source_language.as_deref(),
            &req.target_language,
        )
        .await
    }
}

/// Maps the Swift pair-status code onto a backend availability. Keep in sync
/// with `nagori_apple_translation_pair_status_c` in `Bridge.swift`.
const fn map_pair_status(code: i32) -> BackendAvailability {
    match code {
        // installed
        0 => BackendAvailability::Available,
        // supported but not installed → downloadable language pack
        1 => BackendAvailability::Unavailable(BackendUnavailableReason::AssetMissing),
        // 2 unsupported pair, 3 unknown / no source / timed out
        _ => BackendAvailability::Unavailable(BackendUnavailableReason::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_status_maps_installed_to_available() {
        assert_eq!(map_pair_status(0), BackendAvailability::Available);
    }

    #[test]
    fn pair_status_maps_supported_to_asset_missing() {
        assert_eq!(
            map_pair_status(1),
            BackendAvailability::Unavailable(BackendUnavailableReason::AssetMissing)
        );
    }

    #[test]
    fn pair_status_maps_unsupported_and_unknown_to_unknown() {
        for code in [2, 3, 99] {
            assert_eq!(
                map_pair_status(code),
                BackendAvailability::Unavailable(BackendUnavailableReason::Unknown)
            );
        }
    }

    #[tokio::test]
    async fn availability_is_available_on_macos() {
        assert_eq!(
            AppleTranslateBackend::new().availability().await,
            BackendAvailability::Available
        );
    }

    /// End-to-end smoke test against the live Translation framework. Ignored by
    /// default: the framework can hang or fail outside an app bundle, and the
    /// language pack may not be installed, so this is for manual runs on a
    /// machine with the en→ja pack present — `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires the Translation framework + installed en→ja language pack"]
    async fn real_translate_en_to_ja() {
        let backend = AppleTranslateBackend::new();
        let req = TranslationRequest {
            request_id: nagori_core::RequestId::new(),
            input: "Good morning.".to_owned(),
            source_language: Some("en".to_owned()),
            target_language: "ja".to_owned(),
        };
        match backend.translate(req, CancellationToken::new()).await {
            Ok(out) => assert!(
                !out.text.trim().is_empty(),
                "translation should be non-empty"
            ),
            Err(err) => panic!("translate failed: {err}"),
        }
    }
}

//! Static `(action, provider) → backend` resolution table.
//!
//! The table is the single source of truth for which capability family backs
//! each AI action under each provider. Settings only choose a *provider
//! family*; this table decides whether that resolves to text generation,
//! translation, or embedding — so neither the UI nor the daemon has to encode
//! the dispatch. A linear scan is plenty for a table of this size.

use nagori_core::{AiActionId, AiProviderKind};

/// Which backend capability family handles an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    TextGeneration,
    Translation,
    Embedding,
}

/// One row of the resolution table.
#[derive(Debug, Clone, Copy)]
struct ActionSpec {
    action: AiActionId,
    provider: AiProviderKind,
    backend: BackendKind,
}

/// The wired `(action, provider)` mappings.
///
/// Only actions whose backend is actually implemented appear here; an
/// unlisted pair resolves to `None`, which the engine surfaces as
/// `CapabilityMismatch`. Rows are added as each action's backend lands, so the
/// table doubles as a precise record of what ships today.
const ACTION_SPECS: &[ActionSpec] = &[
    ActionSpec {
        action: AiActionId::Summarize,
        provider: AiProviderKind::AppleNative,
        backend: BackendKind::TextGeneration,
    },
    ActionSpec {
        action: AiActionId::Translate,
        provider: AiProviderKind::AppleNative,
        backend: BackendKind::Translation,
    },
];

/// Resolves the backend family for `(action, provider)`, or `None` when no
/// backend is wired for that combination.
#[must_use]
pub fn resolve_backend(action: AiActionId, provider: AiProviderKind) -> Option<BackendKind> {
    ACTION_SPECS
        .iter()
        .find(|spec| spec.action == action && spec.provider == provider)
        .map(|spec| spec.backend)
}

#[cfg(test)]
mod tests {
    use super::{BackendKind, resolve_backend};
    use nagori_core::{AiActionId, AiProviderKind};

    #[test]
    fn summarize_resolves_to_text_generation_for_apple() {
        assert_eq!(
            resolve_backend(AiActionId::Summarize, AiProviderKind::AppleNative),
            Some(BackendKind::TextGeneration)
        );
    }

    #[test]
    fn translate_resolves_to_translation_for_apple() {
        assert_eq!(
            resolve_backend(AiActionId::Translate, AiProviderKind::AppleNative),
            Some(BackendKind::Translation)
        );
    }

    #[test]
    fn unwired_pairs_resolve_to_none() {
        // Rewrite's backend is not wired yet.
        assert_eq!(
            resolve_backend(AiActionId::Rewrite, AiProviderKind::AppleNative),
            None
        );
        // No provider is configured.
        assert_eq!(
            resolve_backend(AiActionId::Summarize, AiProviderKind::Disabled),
            None
        );
        // OpenAI-compatible has no backend wired yet.
        assert_eq!(
            resolve_backend(AiActionId::Summarize, AiProviderKind::OpenAiCompatible),
            None
        );
    }
}

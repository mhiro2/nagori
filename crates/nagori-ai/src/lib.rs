//! The cross-platform AI engine: provider-agnostic traits, the `(action,
//! provider) → backend` resolver, a deterministic mock backend for tests, and
//! the rule-based quick-action runner.
//!
//! This crate has **no** Apple (or any other platform) dependency. Concrete
//! backends — Apple's Foundation Models / Translation / `NaturalLanguage`
//! bindings — live in `nagori-ai-apple` and implement the [`TextGenerator`] /
//! [`Translator`] / [`Embedder`] traits defined here; the daemon injects them
//! into an [`AiEngine`] at startup.

pub mod backend;
pub mod engine;
pub mod mock;
pub mod quick;
pub mod resolver;
// `redaction` is intentionally crate-private. ARCHITECTURE.md requires
// all redaction to flow through the runtime's `SensitivityClassifier`
// (which combines the built-in patterns with the user-configured
// `regex_denylist`) before any provider sees the input. `Redactor`
// itself only knows the built-in patterns — the quick-action runner runs
// it as a defence-in-depth pass, not as the policy boundary. Exposing
// `Redactor` from this crate would invite future call sites to treat
// it as the boundary and ship pre-redaction strings to a remote
// provider with the user's denylist silently skipped, so we keep the
// module non-public.
pub(crate) mod redaction;

pub use backend::{
    BackendAvailability, BackendUnavailableReason, Embedder, EmbeddingInput, EmbeddingVector,
    TextGenerationCapabilities, TextGenerationRequest, TextGenerator, TranslationOutput,
    TranslationRequest, Translator,
};
pub use engine::{AiActionEngine, AiActionRun, AiEngine, AiEngineBuilder, AiEventStream};
pub use mock::MockBackend;
pub use quick::QuickActionRunner;
pub use resolver::{BackendKind, resolve_backend};

pub mod actions;
pub mod local;
pub mod openai;
pub mod provider;
// `redaction` is intentionally crate-private. ARCHITECTURE.md requires
// all redaction to flow through the runtime's `SensitivityClassifier`
// (which combines the built-in patterns with the user-configured
// `regex_denylist`) before any provider sees the input. `Redactor`
// itself only knows the built-in patterns — `LocalAiProvider` runs it
// as a defence-in-depth pass, not as the policy boundary. Exposing
// `Redactor` from this crate would invite future call sites to treat
// it as the boundary and ship pre-redaction strings to a remote
// provider with the user's denylist silently skipped, so we keep the
// module non-public.
pub(crate) mod redaction;

pub use actions::AiActionRegistry;
pub use local::LocalAiProvider;
pub use openai::StubOpenAiProvider;
pub use provider::{AiProvider, MockAiProvider};

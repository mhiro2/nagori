use nagori_core::policy::redact_text;

/// Built-in pattern redactor. Crate-private so callers cannot reach in and
/// redact strings without also consulting the user's `regex_denylist`; see
/// the `redaction` module comment in `lib.rs` for the policy rationale.
#[derive(Debug, Clone, Default)]
pub(crate) struct Redactor;

impl Redactor {
    // Stateless today, but keep `&self` so callers don't have to retool when
    // we wire in user-loaded patterns; the unused-self lint reasonably fires
    // on a struct with no fields, but the API stability win is worth it.
    #[allow(clippy::unused_self)]
    pub(crate) fn redact(&self, input: &str) -> String {
        redact_text(input)
    }
}

use nagori_core::policy::redact_text;

#[derive(Debug, Clone, Default)]
pub struct Redactor;

impl Redactor {
    pub fn redact(&self, input: &str) -> String {
        redact_text(input)
    }
}

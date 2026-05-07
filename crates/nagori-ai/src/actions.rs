use nagori_core::{AiAction, AiActionId, AiInputPolicy, AiOutputPolicy};

#[derive(Debug, Clone)]
pub struct AiActionRegistry {
    actions: Vec<AiAction>,
}

impl Default for AiActionRegistry {
    fn default() -> Self {
        Self {
            actions: vec![
                action(AiActionId::Summarize, "summarize", false, true),
                action(AiActionId::Translate, "translate", false, true),
                action(AiActionId::FormatJson, "format-json", false, false),
                action(AiActionId::FormatMarkdown, "format-markdown", false, false),
                action(AiActionId::ExplainCode, "explain-code", false, true),
                action(AiActionId::Rewrite, "rewrite", false, true),
                action(AiActionId::ExtractTasks, "extract-tasks", false, true),
                // RedactSecrets is the *redaction* action, but the input
                // policy still says `require_redaction = true`: the local
                // provider falls back to the bare `Redactor` (built-in
                // patterns only), which leaks anything matched by the
                // user's `regex_denylist`. Forcing `require_redaction`
                // routes the input through the settings-aware classifier
                // upstream so user rules apply on Public entries too.
                action(AiActionId::RedactSecrets, "redact-secrets", false, true),
            ],
        }
    }
}

impl AiActionRegistry {
    pub fn all(&self) -> &[AiAction] {
        &self.actions
    }

    pub fn get(&self, id: AiActionId) -> Option<&AiAction> {
        self.actions.iter().find(|action| action.id == id)
    }
}

fn action(id: AiActionId, name: &str, allow_remote: bool, require_redaction: bool) -> AiAction {
    AiAction {
        id,
        name: name.to_owned(),
        input_policy: AiInputPolicy {
            allow_remote,
            require_redaction,
            max_bytes: 64 * 1024,
        },
        output_policy: AiOutputPolicy {
            may_create_entry: true,
        },
    }
}

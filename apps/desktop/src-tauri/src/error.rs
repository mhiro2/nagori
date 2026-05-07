use nagori_core::AppError;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
}

impl CommandError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_input".to_owned(),
            message: message.into(),
            recoverable: true,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "internal_error".to_owned(),
            message: message.into(),
            recoverable: false,
        }
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            code: "unsupported".to_owned(),
            message: message.into(),
            recoverable: false,
        }
    }
}

impl From<AppError> for CommandError {
    fn from(err: AppError) -> Self {
        // Log the full diagnostic detail server-side so we don't lose it,
        // then return a generic user-facing message for variants whose
        // detail can leak DB paths, SQL syntax, regex internals, etc. The
        // frontend uses `code` for i18n lookup and only falls back to
        // `message` when no translation exists, so a generic safe string
        // here matches the existing frontend behaviour while protecting
        // against raw error strings hitting the UI through that fallback.
        tracing::warn!(error = %err, "command_error");
        let recoverable = !matches!(err, AppError::NotFound | AppError::Policy(_));
        Self {
            code: error_code(&err).to_owned(),
            message: user_message(&err),
            recoverable,
        }
    }
}

const fn error_code(err: &AppError) -> &'static str {
    match err {
        AppError::Storage(_) => "storage_error",
        AppError::Search(_) => "search_error",
        AppError::Platform(_) => "platform_error",
        AppError::Permission(_) => "permission_error",
        AppError::Ai(_) => "ai_error",
        AppError::Policy(_) => "policy_error",
        AppError::NotFound => "not_found",
        AppError::InvalidInput(_) => "invalid_input",
        AppError::Unsupported(_) => "unsupported",
    }
}

/// Map an internal `AppError` to a string that is safe to render in the
/// `WebView`. Variants whose detail comes from validated user input
/// (`InvalidInput`, `Unsupported`) or platform diagnostics that we already
/// curated (`Permission`) keep their detail; everything else collapses to
/// a generic, code-keyed sentence so internal paths or query fragments
/// don't leak.
fn user_message(err: &AppError) -> String {
    match err {
        AppError::Storage(_) => "Storage error. Please try again.".to_owned(),
        AppError::Search(_) => "Search failed. Please retry the query.".to_owned(),
        AppError::Platform(_) => "Platform integration failed.".to_owned(),
        AppError::Ai(_) => "AI action failed.".to_owned(),
        AppError::Policy(_) => "Action blocked by policy.".to_owned(),
        AppError::NotFound => "Not found.".to_owned(),
        // Permission/InvalidInput/Unsupported messages are already
        // user-curated (permission hints, hotkey-format errors, etc.) so
        // forwarding them gives the user actionable feedback without
        // leaking implementation detail.
        AppError::Permission(msg) | AppError::InvalidInput(msg) | AppError::Unsupported(msg) => {
            msg.clone()
        }
    }
}

pub type CommandResult<T> = std::result::Result<T, CommandError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_error_keeps_curated_message_and_marks_recoverable() {
        // Permission denials are user-actionable ("grant Accessibility …"),
        // so the curated message must reach the UI verbatim.
        let cmd: CommandError =
            AppError::Permission("Accessibility permission required to auto-paste".to_owned())
                .into();
        assert_eq!(cmd.code, "permission_error");
        assert_eq!(
            cmd.message,
            "Accessibility permission required to auto-paste"
        );
        assert!(cmd.recoverable);
    }

    #[test]
    fn platform_error_collapses_message_but_stays_recoverable() {
        // Platform diagnostics may carry low-level detail (CFRunLoop, IOKit
        // codes, …), so we collapse to a generic message but the error is
        // still treated as transient.
        let cmd: CommandError = AppError::Platform("CGEventPostToPid failed".to_owned()).into();
        assert_eq!(cmd.code, "platform_error");
        assert_eq!(cmd.message, "Platform integration failed.");
        assert!(cmd.recoverable);
    }

    #[test]
    fn auto_paste_failure_surfaces_as_recoverable_command_error() {
        // Mirrors `paste_entry_from_palette`: when `paste_frontmost` fails,
        // the runtime returns AppError::Platform / Permission. The
        // command layer wraps it via `?`, so the conversion path matters
        // for the toast the palette renders.
        let cmd: CommandError = AppError::Permission("paste blocked".to_owned()).into();
        assert!(cmd.recoverable, "auto-paste failure must be recoverable");
        assert_eq!(cmd.code, "permission_error");
    }

    #[test]
    fn not_found_is_irrecoverable() {
        // The frontend special-cases `not_found` to clear stale rows from the
        // palette rather than show a retry toast, so the variant must keep
        // the `not_found` code and `recoverable: false` flag.
        let cmd: CommandError = AppError::NotFound.into();
        assert_eq!(cmd.code, "not_found");
        assert!(!cmd.recoverable);
    }

    #[test]
    fn policy_error_collapses_to_generic_message_and_irrecoverable() {
        // Policy denials carry rule names that aren't useful (and may leak
        // denylist patterns) — the user-facing message stays generic.
        let cmd: CommandError = AppError::Policy("regex denylist hit".to_owned()).into();
        assert_eq!(cmd.code, "policy_error");
        assert_eq!(cmd.message, "Action blocked by policy.");
        assert!(!cmd.recoverable);
    }

    #[test]
    fn invalid_input_forwards_curated_detail() {
        // `invalid_input` messages are crafted by the command layer (hotkey
        // format, entry id parse, …) so they are safe to surface verbatim.
        let cmd: CommandError = AppError::InvalidInput("invalid hotkey: Cmd+".to_owned()).into();
        assert_eq!(cmd.code, "invalid_input");
        assert_eq!(cmd.message, "invalid hotkey: Cmd+");
        assert!(cmd.recoverable);
    }

    #[test]
    fn storage_and_search_messages_are_generic() {
        // SQL-shaped detail (paths, statements, FTS column names) must not
        // reach the WebView — those errors collapse to a generic prompt.
        let storage: CommandError = AppError::Storage("disk I/O".to_owned()).into();
        assert_eq!(storage.code, "storage_error");
        assert_eq!(storage.message, "Storage error. Please try again.");
        let search: CommandError = AppError::Search("syntax error".to_owned()).into();
        assert_eq!(search.code, "search_error");
        assert_eq!(search.message, "Search failed. Please retry the query.");
    }

    #[test]
    fn invalid_input_constructor_marks_recoverable() {
        let err = CommandError::invalid_input("nope");
        assert_eq!(err.code, "invalid_input");
        assert!(err.recoverable);
        let internal = CommandError::internal("explosion");
        assert_eq!(internal.code, "internal_error");
        assert!(!internal.recoverable);
    }
}

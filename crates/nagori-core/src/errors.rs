use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("storage error: {0}")]
    Storage(String),
    #[error("search error: {0}")]
    Search(String),
    #[error("platform error: {0}")]
    Platform(String),
    #[error("permission error: {0}")]
    Permission(String),
    #[error("ai error: {0}")]
    Ai(String),
    #[error("policy error: {0}")]
    Policy(String),
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("configuration error: {0}")]
    Configuration(String),
    /// An optimistic-concurrency check failed: the caller's snapshot was based
    /// on a stale revision and writing it would clobber a concurrent change.
    /// The caller is expected to refresh and retry rather than treat this as a
    /// hard failure. Used by the settings compare-and-swap save path.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Auto-paste (synthetic Cmd/Ctrl+V) failed. Carries a classified
    /// [`PasteFailureReason`] alongside the diagnostic message so surfaces can
    /// render a targeted hint instead of a raw string. The clipboard write
    /// itself has already succeeded by the time this is raised, so callers
    /// keep the "copy succeeded — paste manually" framing.
    #[error("paste error: {message}")]
    Paste {
        reason: PasteFailureReason,
        message: String,
    },
}

/// Why an auto-paste attempt failed.
///
/// Platform adapters tag the failures they raise (`AccessibilityMissing` on
/// macOS, `ToolMissing` / `Timeout` on Linux Wayland, …); the desktop command
/// layer adds `PreviousAppLost` for the focus-restore step that lives above
/// the platform adapter. The variants map onto the same remediation surfaces
/// `nagori doctor` already reports (Accessibility permission, the `wtype`
/// external tool), so the UI hint and the doctor diagnostic stay consistent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteFailureReason {
    /// The OS grant that gates synthetic input is missing (macOS Accessibility).
    AccessibilityMissing,
    /// An external tool the platform shells out to for paste is not installed
    /// (Linux Wayland `wtype`).
    ToolMissing { tool: String },
    /// The paste tool / synthesis call ran but did not return in time.
    Timeout,
    /// Synthetic paste is not available on this platform or build at all.
    SynthUnsupported,
    /// The app the user copied from could not be re-focused before the
    /// synthesised keystroke, so the paste would have landed in Nagori itself.
    PreviousAppLost,
    /// Anything else — e.g. Windows `SendInput` rejected by UIPI, or an
    /// unexpected internal failure on the paste path.
    Unknown,
}

impl PasteFailureReason {
    /// Stable machine token surfaced to the frontend (camelCase to match the
    /// DTO convention) and reused as the i18n lookup key for the UI hint.
    #[must_use]
    pub const fn token(&self) -> &'static str {
        match self {
            Self::AccessibilityMissing => "accessibilityMissing",
            Self::ToolMissing { .. } => "toolMissing",
            Self::Timeout => "timeout",
            Self::SynthUnsupported => "synthUnsupported",
            Self::PreviousAppLost => "previousAppLost",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_failure_reason_tokens_are_stable_camel_case() {
        // The frontend keys its i18n hint + StatusBar chip off these tokens,
        // so they are a wire contract: renaming one without updating the UI
        // silently drops the hint. `tool` rides alongside the token for
        // `ToolMissing`, so the token itself stays tool-agnostic.
        assert_eq!(
            PasteFailureReason::AccessibilityMissing.token(),
            "accessibilityMissing"
        );
        assert_eq!(
            PasteFailureReason::ToolMissing {
                tool: "wtype".to_owned()
            }
            .token(),
            "toolMissing"
        );
        assert_eq!(PasteFailureReason::Timeout.token(), "timeout");
        assert_eq!(
            PasteFailureReason::SynthUnsupported.token(),
            "synthUnsupported"
        );
        assert_eq!(
            PasteFailureReason::PreviousAppLost.token(),
            "previousAppLost"
        );
        assert_eq!(PasteFailureReason::Unknown.token(), "unknown");
    }
}

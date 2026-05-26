use async_trait::async_trait;
use nagori_core::Result;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionKind {
    Accessibility,
    InputMonitoring,
    Clipboard,
    Notifications,
    AutoLaunch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionState {
    Granted,
    Denied,
    NotDetermined,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionStatus {
    pub kind: PermissionKind,
    pub state: PermissionState,
    /// Short English diagnostic summary (e.g. `"Accessibility not trusted"`).
    /// Intended for CLI / log consumption; user-facing UI now renders its own
    /// localized copy from the Setup card, so the message stays terse.
    pub message: Option<String>,
    /// Stable `snake_case` identifier so downstream scripts can branch on
    /// the reason without parsing the message string. Examples:
    /// `"accessibility_not_trusted"`, `"accessibility_not_prompted"`.
    pub reason_code: Option<String>,
    /// Frontend deep-link hint pointing at the Setup tab card that
    /// resolves this status (e.g. `"setup/accessibility"`). Ignored by
    /// the CLI; used by HTML mail templates / support tooling to send
    /// users straight to the right card.
    pub setup_route: Option<String>,
    /// Permalink to the relevant docs section, when one exists. Kept
    /// separate from `message` so consumers can render it as a hyperlink
    /// rather than re-extracting a URL from prose.
    pub docs_url: Option<String>,
}

/// Context passed into [`PermissionChecker::check`] so the per-OS
/// implementation can distinguish "we have never prompted the user" from
/// "we prompted them and they declined or revoked".
///
/// Today this only carries the macOS `accessibility_prompted_at`
/// timestamp; the struct exists rather than a bare `Option` so future
/// permissions (Notifications, `InputMonitoring`) can extend the context
/// without churning every caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionCheckContext {
    /// Persisted timestamp of the first
    /// `AXIsProcessTrustedWithOptions(prompt: true)` call. `None` means
    /// the macOS checker has never asked the OS to surface the
    /// Accessibility dialog for this install — distinct from "asked and
    /// the user declined", which keeps the timestamp set.
    pub accessibility_prompted_at: Option<OffsetDateTime>,
}

#[async_trait]
pub trait PermissionChecker: Send + Sync {
    /// Probe each permission and report the current state. `ctx` carries
    /// onboarding bookkeeping that some platforms (notably macOS) need
    /// to discriminate `NotDetermined` from `Denied`.
    async fn check(&self, ctx: &PermissionCheckContext) -> Result<Vec<PermissionStatus>>;
    /// Trigger the host's accessibility prompt (where one exists) and
    /// return the *current* trust state. The prompt itself is
    /// asynchronous: a `Denied` result with `prompt = true` means
    /// "the OS dialog is now showing or has been suppressed by TCC",
    /// not "the user actively declined". Callers persist
    /// `accessibility_prompted_at` themselves; the checker stays
    /// stateless apart from its own FFI calls.
    async fn request_accessibility(&self, prompt: bool) -> Result<PermissionStatus>;
}

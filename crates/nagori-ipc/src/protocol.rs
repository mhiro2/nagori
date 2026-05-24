use nagori_core::{
    AiActionId, AiOutput, AppSettings, ClipboardEntry, ContentKind, EntryId, PasteFormat,
    RankReason, RepresentationRole, RepresentationSummary, SearchResult, Sensitivity,
    safe_preview_for_dto,
};
use nagori_platform::PlatformCapabilities;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Wire-level envelope wrapping every IPC request with the per-launch auth token.
///
/// Every connection ships a single line of JSON whose shape is
/// `{"token": "<hex>", "request": <IpcRequest>}`. The daemon validates `token`
/// in constant time before dispatching `request`; clients without the
/// per-launch token cannot reach any handler — including `Health` and
/// `Shutdown`. Adding the wrapper at the protocol layer (vs the server layer)
/// keeps tests, traces, and any future transports honest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEnvelope {
    pub token: String,
    pub request: IpcRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcRequest {
    Search(SearchRequest),
    GetEntry(GetEntryRequest),
    ListRecent(ListRecentRequest),
    ListPinned(ListPinnedRequest),
    CopyEntry(CopyEntryRequest),
    PasteEntry(PasteEntryRequest),
    AddEntry(AddEntryRequest),
    DeleteEntry(DeleteEntryRequest),
    PinEntry(PinEntryRequest),
    RunAiAction(RunAiActionRequest),
    GetSettings,
    UpdateSettings(UpdateSettingsRequest),
    Clear(ClearRequest),
    Doctor,
    Health,
    /// Static report of what the host adapter can do (clipboard /
    /// paste / hotkey / etc.). Read-only and cheap to answer; the
    /// daemon's dispatcher treats it as a control request, so CLI
    /// callers can probe it even when `cli_ipc_enabled` is false.
    Capabilities,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcResponse {
    Search(SearchResponse),
    Entry(EntryDto),
    Entries(Vec<EntryDto>),
    Settings(AppSettings),
    AiOutput(AiOutputDto),
    Cleared(ClearResponse),
    Doctor(DoctorReport),
    Capabilities(Box<PlatformCapabilities>),
    Ack,
    Error(IpcError),
    Health(HealthResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetEntryRequest {
    pub id: EntryId,
    pub include_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRecentRequest {
    pub limit: usize,
    #[serde(default)]
    pub include_sensitive: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListPinnedRequest {
    /// Whether to include the raw text body for `Private`/`Secret` entries.
    /// Defaults to `false` so a client that omits the field still gets the
    /// safe "preview only" behaviour — matching the default `ListRecent`
    /// treatment of sensitive payloads.
    #[serde(default)]
    pub include_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyEntryRequest {
    pub id: EntryId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasteEntryRequest {
    pub id: EntryId,
    #[serde(default)]
    pub format: Option<PasteFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddEntryRequest {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteEntryRequest {
    pub id: EntryId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinEntryRequest {
    pub id: EntryId,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAiActionRequest {
    pub id: EntryId,
    pub action: AiActionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSettingsRequest {
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClearRequest {
    /// Wipe every unpinned entry.
    All,
    /// Wipe unpinned entries older than `days` days.
    OlderThanDays { days: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearResponse {
    pub deleted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub version: String,
    pub db_path: String,
    pub socket_path: String,
    pub capture_enabled: bool,
    pub auto_paste_enabled: bool,
    pub ai_enabled: bool,
    /// Surfaces the only background network-touching setting the daemon
    /// honours today. Doctor prints it so operators can see at a glance
    /// what (if anything) is allowed to reach the network. `false` means
    /// neither the startup probe nor doctor's `latest_version` lookup
    /// run; the manual "Check for updates now" button still works.
    #[serde(default = "default_auto_update_check_report")]
    pub auto_update_check: bool,
    pub ai_provider: String,
    pub permissions: Vec<DoctorPermission>,
    /// Health snapshot of the background maintenance loop. Surfaced here
    /// so `nagori doctor` flags retention pauses without the operator
    /// having to grep tracing logs.
    #[serde(default)]
    pub maintenance: MaintenanceHealthReport,
    /// Steady-state health snapshot of the capture loop. Where `startup`
    /// covers the one-shot pre-poll init, this row captures per-tick
    /// outcomes once the loop is polling: degraded counter, last
    /// success / non-success timestamps, and the category of the most
    /// recent non-success outcome. Surfaced here so a silently filtering
    /// loop is visible in `nagori doctor` without grepping logs.
    #[serde(default)]
    pub capture: CaptureHealthReport,
    /// Health snapshot of the IPC server's per-connection handlers.
    /// Mirrors the field on `HealthResponse` so operators running
    /// `nagori doctor` get the same panic visibility as automated
    /// `nagori health` probes.
    #[serde(default)]
    pub ipc: IpcHealthReport,
    /// Outcome of desktop startup's settings-load gate. Covers both the
    /// capture loop's pre-poll initialisation and the settings subscriber's
    /// initial `get_settings()` — either one aborting on a failed load
    /// degrades the gated "ready" notification. `ready` stays `false`
    /// until the runtime hosting these tasks posts its outcome, and is
    /// first-outcome-wins so a subscriber-only failure sticks even if
    /// the capture task later loads settings on its own retry. The
    /// desktop's `setup()` fires the "ready" notification only after
    /// this flips to `ready=true`, so `nagori doctor` and the
    /// notification share one source of truth instead of drifting.
    #[serde(default)]
    pub startup: StartupHealthReport,
    /// Active update channel (e.g. `"stable"`).
    #[serde(default)]
    pub update_channel: String,
    /// Latest released version discovered by the daemon, if a probe
    /// against the GitHub Releases API succeeded. `None` when the probe
    /// is disabled (offline mode), times out, or fails — `nagori doctor`
    /// surfaces that as "(unknown)".
    #[serde(default)]
    pub latest_version: Option<String>,
    /// Aggregate byte count currently held in the `entry_thumbnails`
    /// derived cache. Surfaced here so operators can verify the LRU
    /// budget is being respected without dropping to SQL. `None` when
    /// the daemon could not read the stat (e.g. legacy schema).
    #[serde(default)]
    pub thumbnail_total_bytes: Option<u64>,
    /// Configured upper bound for `thumbnail_total_bytes`. `None` means
    /// the operator disabled the LRU sweep; positive values are the
    /// active budget in bytes.
    #[serde(default)]
    pub thumbnail_budget_bytes: Option<u64>,
}

const fn default_auto_update_check_report() -> bool {
    // A legacy daemon reply that predates this field still drove the
    // updater probe by default — match that here so the absence in JSON
    // doesn't surface as "network disabled" in `nagori doctor`.
    true
}

/// Categorisation of the most recent non-success outcome observed by the
/// capture loop.
///
/// The values cover the failure surfaces that drive silent data loss
/// differently — adapter / settings-load / storage errors mean the loop
/// lost a clip even though it tried to land one, while policy /
/// oversized drops mean the loop saw the clip but refused it. The UI
/// uses the category to render a tailored hint (re-grant Accessibility,
/// raise `max_entry_size_bytes`, loosen `regex_denylist`, check disk
/// space, …) instead of a generic "degraded".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEventCategory {
    /// Settings could not be loaded / a runtime `SensitivityClassifier`
    /// rebuild failed (e.g. uncompilable `regex_denylist`). Treated as
    /// an error: the loop will keep retrying but every clip is silently
    /// failing classification.
    SettingsLoad,
    /// Reader / window adapter call returned an error (pasteboard read,
    /// AX query, snapshot capture). The loop cannot tell whether
    /// anything is on the clipboard, so this is the worst-case silent
    /// data loss.
    Adapter,
    /// Storage layer (`SqliteStore::insert` / blocking task) returned an
    /// error. The loop saw the clip and classified it successfully but
    /// could not persist it — disk full, DB locked, schema migration
    /// blocker. Surfaced separately from `Adapter` so the UI can point
    /// at storage diagnostics rather than re-granting clipboard
    /// permissions.
    Storage,
    /// Capture policy refused the clip — `Blocked` sensitivity, secret
    /// handling = `block`, or a `kind_disabled` filter. Not an error;
    /// the loop did its job. Surfaced so the UI can explain "we saw
    /// the clip but it matched your denylist".
    Policy,
    /// Payload exceeded `max_entry_size_bytes` (either at pre-read or
    /// after the body landed). Not an error; the loop deliberately
    /// dropped the clip. Surfaced so the UI can suggest raising the
    /// limit when the user expected the clip to land.
    OversizedDrop,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureHealthReport {
    /// Wall-clock time of the most recent successful `capture_once`
    /// tick. `None` until the first one lands — combined with `degraded`
    /// this lets the UI render "never captured" vs. "stopped capturing
    /// 12 minutes ago".
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_success_at: Option<OffsetDateTime>,
    /// Consecutive error-class outcomes (`Adapter` / `SettingsLoad` /
    /// `Storage`). Reset on the next success. Drops (`Policy` /
    /// `OversizedDrop`) do not contribute because they're intentional.
    pub consecutive_failures: u32,
    /// `true` once `consecutive_failures` crosses the daemon's degraded
    /// threshold; cleared on the next successful tick.
    pub degraded: bool,
    /// Most recent error-class message, if any. Cleared on the next
    /// success so the UI can stop showing a stale error after a flake
    /// recovers.
    pub last_error: Option<String>,
    /// Category of the most recent non-success outcome (error *or*
    /// drop). Stable across the degraded window so the UI keeps showing
    /// a consistent "why" until a non-success outcome of a different
    /// kind lands.
    pub last_event_category: Option<CaptureEventCategory>,
    /// Wall-clock time of the most recent non-success outcome.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_event_at: Option<OffsetDateTime>,
}

/// Wire-format snapshot of IPC server-side handler outcomes.
///
/// The IPC dispatcher runs per-connection handlers on a `JoinSet`; without
/// an explicit observer, the `join_next()` reap drops the `Result`, so a
/// panicking handler would otherwise be invisible to operators. This
/// report exposes the cumulative panic count (and most-recent panic
/// message) so a degraded IPC surface is visible in `nagori health` and
/// `nagori doctor` without grepping logs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcHealthReport {
    /// Cumulative count of IPC handler tasks observed to panic since the
    /// daemon started. Saturating add so a permanent panic loop plateaus
    /// at `u64::MAX` instead of wrapping back to zero.
    pub handler_panic_count: u64,
    /// Most recent panic message (Display of `tokio::task::JoinError`),
    /// already routed through the daemon's hex-token redactor so
    /// auth-token-shaped substrings never surface here. Sticky after
    /// the first panic so transient log pressure cannot erase it — a
    /// follow-up panic overwrites with the latest message.
    pub last_panic_message: Option<String>,
    /// Number of IPC handler panics observed inside the most recent
    /// 5-minute window. Lets dashboards distinguish "panic loop still
    /// firing" from "one fluke an hour ago" — the cumulative
    /// `handler_panic_count` alone collapses both into a single number.
    #[serde(default)]
    pub panics_last_5m: u32,
    /// Active ceiling on concurrent IPC handlers (the semaphore size
    /// the daemon was started with). `0` indicates a legacy daemon
    /// that did not populate this field, or that the local accept loop
    /// has not yet stamped its config — readers should render that as
    /// "(unknown)" rather than treating it as a real limit.
    #[serde(default)]
    pub max_concurrent_connections: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceHealthReport {
    /// Consecutive failed runs of the maintenance loop. Resets to zero
    /// on the next successful run.
    pub consecutive_failures: u32,
    /// `true` once `consecutive_failures` crosses the daemon's degraded
    /// threshold; cleared on the next successful run.
    pub degraded: bool,
    /// Most recent failure message, if any. Kept stable across the
    /// degraded window so doctor / health output is reproducible.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StartupHealthReport {
    /// `true` once the desktop's startup gate posts a successful
    /// outcome (today: the capture loop loaded settings and entered
    /// polling). `false` while initialisation is still pending and
    /// after a recorded failure — callers should treat `!ready &&
    /// last_error.is_none()` as "still initialising".
    pub ready: bool,
    /// Most recent startup-init failure message, if any. Sticky after
    /// the first outcome so transient retries can't mask the original
    /// abort reason (see `StartupHealth::record_capture_failed`). A
    /// subscriber-side failure can land here even if the capture task
    /// itself never aborted, because either failure means the desktop
    /// isn't fully running.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorPermission {
    pub kind: String,
    pub state: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
    /// Health snapshot of the background maintenance loop. Cheap to
    /// serialise even when nothing is wrong (default-zero), and gives
    /// callers (`nagori doctor`, dashboards, oncall checks) a single
    /// place to learn that retention has stopped advancing.
    #[serde(default)]
    pub maintenance: MaintenanceHealthReport,
    /// Health snapshot of the capture loop's steady-state polling.
    /// Lets dashboards distinguish "retention is wedged" from "every
    /// clip is being dropped" without a second IPC roundtrip.
    #[serde(default)]
    pub capture: CaptureHealthReport,
    /// Health snapshot of the IPC server's per-connection handlers.
    /// Surfaces handler panics that would otherwise be silently dropped
    /// by `JoinSet::join_next()` so dashboards can distinguish a healthy
    /// daemon from one whose IPC dispatchers are panicking on a hot
    /// request shape.
    #[serde(default)]
    pub ipc: IpcHealthReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultDto {
    pub id: EntryId,
    pub kind: ContentKind,
    pub preview: String,
    pub score: f32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub pinned: bool,
    #[serde(default)]
    pub sensitivity: Sensitivity,
    #[serde(default)]
    pub rank_reasons: Vec<RankReason>,
    #[serde(default)]
    pub source_app_name: Option<String>,
    #[serde(default)]
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl From<SearchResult> for SearchResultDto {
    fn from(value: SearchResult) -> Self {
        Self {
            id: value.entry_id,
            kind: value.content_kind,
            preview: value.preview,
            score: value.score,
            created_at: value.created_at,
            pinned: value.pinned,
            sensitivity: value.sensitivity,
            rank_reasons: value.rank_reason,
            source_app_name: value.source_app_name,
            representation_summary: Vec::new(),
        }
    }
}

impl SearchResultDto {
    #[must_use]
    pub fn with_representation_summaries(mut self, summaries: &[RepresentationSummary]) -> Self {
        self.representation_summary = summaries
            .iter()
            .map(RepresentationSummaryDto::from_summary)
            .collect();
        self
    }
}

/// Wire-safe projection of one row from `entry_representations`.
///
/// Bytes and text never cross the IPC boundary — the desktop frontend only
/// needs to know what flavours the entry preserved (so it can render
/// "HTML + Plain" badges, "Preserved formats:" lists, etc.) and how big
/// each one was. The blob itself is fetched lazily through the dedicated
/// preview / image-scheme paths when the user actually selects an entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepresentationSummaryDto {
    pub mime_type: String,
    pub role: RepresentationRole,
    pub byte_count: u64,
}

impl RepresentationSummaryDto {
    pub fn from_summary(summary: &RepresentationSummary) -> Self {
        Self {
            mime_type: summary.mime_type.clone(),
            role: summary.role,
            byte_count: summary.byte_count,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryDto {
    pub id: EntryId,
    pub kind: ContentKind,
    pub text: Option<String>,
    pub preview: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    pub use_count: u32,
    pub pinned: bool,
    pub source_app_name: Option<String>,
    #[serde(default)]
    pub sensitivity: Sensitivity,
    #[serde(default)]
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl EntryDto {
    pub fn from_entry(entry: ClipboardEntry, include_text: bool) -> Self {
        let preview = safe_preview_for_dto(&entry);
        Self {
            id: entry.id,
            kind: entry.content_kind(),
            text: include_text.then(|| entry.plain_text().unwrap_or_default().to_owned()),
            preview,
            created_at: entry.metadata.created_at,
            updated_at: entry.metadata.updated_at,
            last_used_at: entry.metadata.last_used_at,
            use_count: entry.metadata.use_count,
            pinned: entry.lifecycle.pinned,
            source_app_name: entry.metadata.source.and_then(|source| source.name),
            sensitivity: entry.sensitivity,
            representation_summary: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_representation_summaries(mut self, summaries: &[RepresentationSummary]) -> Self {
        self.representation_summary = summaries
            .iter()
            .map(RepresentationSummaryDto::from_summary)
            .collect();
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiOutputDto {
    pub text: String,
    pub created_entry: Option<EntryId>,
    pub warnings: Vec<String>,
}

impl From<AiOutput> for AiOutputDto {
    fn from(value: AiOutput) -> Self {
        Self {
            text: value.text,
            created_entry: value.created_entry,
            warnings: value.warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pre-`startup`-field `DoctorReport` JSON must still deserialize against
    /// the current shape so a newer CLI can read a `Doctor` reply from an
    /// older daemon without a wire break. The reverse direction (newer daemon,
    /// older CLI) is covered by `deserializes_unknown_startup_field_is_ignored`.
    #[test]
    fn deserializes_doctor_report_without_startup_field() {
        let legacy_json = r#"{
            "version": "0.1.0",
            "db_path": "/tmp/db.sqlite",
            "socket_path": "/tmp/sock",
            "capture_enabled": true,
            "auto_paste_enabled": false,
            "ai_enabled": false,
            "local_only_mode": false,
            "ai_provider": "none",
            "permissions": []
        }"#;
        let report: DoctorReport =
            serde_json::from_str(legacy_json).expect("legacy DoctorReport must still deserialize");
        assert!(!report.startup.ready);
        assert!(report.startup.last_error.is_none());
        assert!(!report.maintenance.degraded);
        assert_eq!(report.maintenance.consecutive_failures, 0);
    }

    /// `DoctorReport` must keep accepting unknown JSON fields so a newer
    /// daemon can ship extra health rows without breaking older CLI
    /// builds — this pins the *current* type's behaviour rather than a
    /// hypothetical frozen legacy shape. If someone later adds
    /// `#[serde(deny_unknown_fields)]` to `DoctorReport`, this test
    /// trips and forces a deliberate wire-break decision.
    #[test]
    fn doctor_report_ignores_unknown_top_level_fields() {
        let future_json = r#"{
            "version": "0.3.0",
            "db_path": "/tmp/db.sqlite",
            "socket_path": "/tmp/sock",
            "capture_enabled": true,
            "auto_paste_enabled": false,
            "ai_enabled": false,
            "local_only_mode": false,
            "ai_provider": "none",
            "permissions": [],
            "future_health_row": {"foo": "bar"},
            "another_unknown": 42
        }"#;
        let report: DoctorReport = serde_json::from_str(future_json)
            .expect("DoctorReport must silently ignore unknown fields");
        assert_eq!(report.version, "0.3.0");
        assert!(!report.startup.ready);
    }

    /// Equivalent guard for the nested `StartupHealthReport`: extra
    /// fields within `startup` must be tolerated so the inner schema
    /// can grow (e.g. an `error_code` companion to `last_error`).
    #[test]
    fn startup_health_report_ignores_unknown_inner_fields() {
        let future_json = r#"{
            "ready": false,
            "last_error": "settings load failed",
            "error_code": "storage_error",
            "retries": 3
        }"#;
        let parsed: StartupHealthReport = serde_json::from_str(future_json)
            .expect("StartupHealthReport must silently ignore unknown fields");
        assert!(!parsed.ready);
        assert_eq!(parsed.last_error.as_deref(), Some("settings load failed"));
    }

    /// Full round-trip of a populated `startup` field — ensures the
    /// serialized shape matches the schema embedded in `nagori doctor`
    /// JSON output and the desktop notification gate.
    #[test]
    fn startup_health_report_round_trips() {
        let original = StartupHealthReport {
            ready: false,
            last_error: Some("settings load failed".to_owned()),
        };
        let json = serde_json::to_string(&original).expect("StartupHealthReport must serialize");
        assert!(json.contains("\"ready\":false"));
        assert!(json.contains("\"last_error\":\"settings load failed\""));
        let parsed: StartupHealthReport =
            serde_json::from_str(&json).expect("StartupHealthReport must round-trip");
        assert!(!parsed.ready);
        assert_eq!(parsed.last_error.as_deref(), Some("settings load failed"));
    }
}

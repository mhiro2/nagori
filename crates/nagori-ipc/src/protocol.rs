use nagori_core::{
    AiActionId, AiOutput, AppSettings, ClipboardEntry, ContentKind, EntryId, PasteFormat,
    RankReason, SearchResult, Sensitivity, safe_preview_for_dto,
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
    pub local_only_mode: bool,
    pub ai_provider: String,
    pub permissions: Vec<DoctorPermission>,
    /// Health snapshot of the background maintenance loop. Surfaced here
    /// so `nagori doctor` flags retention pauses without the operator
    /// having to grep tracing logs.
    #[serde(default)]
    pub maintenance: MaintenanceHealthReport,
    /// Active update channel (e.g. `"stable"`).
    #[serde(default)]
    pub update_channel: String,
    /// Latest released version discovered by the daemon, if a probe
    /// against the GitHub Releases API succeeded. `None` when the probe
    /// is disabled (offline mode), times out, or fails — `nagori doctor`
    /// surfaces that as "(unknown)".
    #[serde(default)]
    pub latest_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        }
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

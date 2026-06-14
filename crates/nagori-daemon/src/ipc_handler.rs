use nagori_core::{
    AiProviderKind, AppError, AppSettings, ClipboardEntry, EntryId, EntryRepository,
    MAX_ENTRY_SIZE_BYTES, RepresentationSummary, Result, SearchQuery,
    is_text_safe_for_default_output,
};
use nagori_ipc::{
    AddEntryRequest, AiOutputDto, ClearRequest, ClearResponse, CopyEntryRequest,
    DeleteEntryRequest, DoctorPermission, DoctorReport, EntryDto, GetEntryRequest, HealthResponse,
    IpcError, IpcRequest, IpcResponse, ListPinnedRequest, ListRecentRequest, PasteEntryRequest,
    PinEntryRequest, RunAiActionRequest, RunQuickActionRequest, SearchRequest, SearchResponse,
    SearchResultDto, SettingsResponse, UpdateSettingsRequest,
};
use nagori_platform::PermissionCheckContext;
use nagori_search::normalize_text;
use std::collections::HashMap;
use std::time::Instant;
use time::OffsetDateTime;

use crate::runtime::{NagoriRuntime, elapsed_ms};

impl NagoriRuntime {
    pub async fn handle_ipc(&self, request: IpcRequest) -> IpcResponse {
        // Single observability point for every IPC request. We log only the
        // request *kind* (an enum discriminant, never the payload), the
        // outcome code, and the wall-clock cost — no entry text, query
        // string, or settings blob — so operators can spot slow or failing
        // request classes without the log capturing clipboard contents.
        let kind = request_kind(&request);
        let started = Instant::now();
        let result = self.handle_ipc_result(request).await;
        let result_code = match &result {
            Ok(_) => "ok",
            Err(err) => error_code(err),
        };
        tracing::debug!(
            request_kind = kind,
            result_code,
            elapsed_ms = elapsed_ms(started),
            "ipc_request"
        );
        match result {
            Ok(response) => response,
            Err(err) => IpcResponse::Error(IpcError {
                code: error_code(&err).to_owned(),
                message: err.to_string(),
                recoverable: !matches!(
                    err,
                    AppError::NotFound | AppError::Policy(_) | AppError::Configuration(_)
                ),
            }),
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_ipc_result(&self, request: IpcRequest) -> Result<IpcResponse> {
        if !self.with_settings(|settings| settings.cli_ipc_enabled)
            && !is_ipc_control_request(&request)
        {
            return Err(AppError::Permission(
                "CLI IPC is disabled in settings".to_owned(),
            ));
        }
        match request {
            IpcRequest::Search(SearchRequest { query, limit }) => {
                let results = self
                    .search(SearchQuery::new(&query, normalize_text(&query), limit))
                    .await?;
                let ids: Vec<_> = results.iter().map(|r| r.entry_id).collect();
                let summaries = self.store.list_representation_summaries(&ids).await?;
                let dtos = results
                    .into_iter()
                    .map(|result| {
                        let entry_id = result.entry_id;
                        let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
                        SearchResultDto::from(result).with_representation_summaries(reps)
                    })
                    .collect();
                Ok(IpcResponse::Search(SearchResponse { results: dtos }))
            }
            IpcRequest::GetEntry(GetEntryRequest {
                id,
                include_sensitive,
            }) => {
                let entry = self.get_entry(id).await?.ok_or(AppError::NotFound)?;
                let include_text =
                    include_sensitive || is_text_safe_for_default_output(entry.sensitivity);
                Ok(IpcResponse::Entry(
                    self.hydrate_entry_dto(entry, include_text).await?,
                ))
            }
            IpcRequest::ListRecent(ListRecentRequest {
                limit,
                include_sensitive,
            }) => {
                let entries = self.list_recent(limit).await?;
                let dtos = self.hydrate_entry_dtos(entries, include_sensitive).await?;
                Ok(IpcResponse::Entries(dtos))
            }
            IpcRequest::ListPinned(ListPinnedRequest { include_sensitive }) => {
                let entries = self.list_pinned().await?;
                let dtos = self.hydrate_entry_dtos(entries, include_sensitive).await?;
                Ok(IpcResponse::Entries(dtos))
            }
            IpcRequest::AddEntry(AddEntryRequest { text }) => {
                let id = self.add_text(text).await?;
                self.notify_external_mutation();
                let entry = self.get_entry(id).await?.ok_or(AppError::NotFound)?;
                let include_text = is_text_safe_for_default_output(entry.sensitivity);
                Ok(IpcResponse::Entry(
                    self.hydrate_entry_dto(entry, include_text).await?,
                ))
            }
            // Copy / paste also count as corpus mutations: they bump the
            // entry's use_count / last_used_at, which reorders ranking. The
            // clipboard write usually re-triggers the host's capture loop
            // (whose notifier refreshes the palette), but not when capture
            // is disabled or policy drops the event — so notify explicitly.
            IpcRequest::CopyEntry(CopyEntryRequest { id }) => {
                self.copy_entry(id).await?;
                self.notify_external_mutation();
                Ok(IpcResponse::Ack)
            }
            IpcRequest::PasteEntry(PasteEntryRequest { id, format }) => {
                self.paste_entry(id, format).await?;
                self.notify_external_mutation();
                Ok(IpcResponse::Ack)
            }
            IpcRequest::DeleteEntry(DeleteEntryRequest { id }) => {
                self.delete_entry(id).await?;
                self.notify_external_mutation();
                Ok(IpcResponse::Ack)
            }
            IpcRequest::PinEntry(PinEntryRequest { id, pinned }) => {
                self.pin_entry(id, pinned).await?;
                self.notify_external_mutation();
                Ok(IpcResponse::Ack)
            }
            IpcRequest::RunQuickAction(RunQuickActionRequest { id, action }) => {
                let output = self.run_quick_action(id, action).await?;
                Ok(IpcResponse::AiOutput(AiOutputDto::from(output)))
            }
            IpcRequest::RunAiAction(RunAiActionRequest {
                id,
                action,
                options,
            }) => {
                let output = self.run_ai_action(id, action, options).await?;
                Ok(IpcResponse::AiOutput(AiOutputDto::from(output)))
            }
            IpcRequest::GetSettings => {
                // Return the revision alongside the blob so an IPC client can
                // echo it back as `expected_revision` for a compare-and-swap
                // update — the read half of the lost-update protection.
                let (value, revision) = self.get_settings_with_revision().await?;
                Ok(IpcResponse::Settings(SettingsResponse { value, revision }))
            }
            IpcRequest::UpdateSettings(UpdateSettingsRequest {
                value,
                expected_revision,
            }) => {
                let settings: AppSettings = serde_json::from_value(value)
                    .map_err(|err| AppError::InvalidInput(err.to_string()))?;
                // Route through the compare-and-swap save when the client
                // supplied a revision so a stale full-blob write can't clobber a
                // concurrent single-field change; fall back to the unconditional
                // save for clients that don't track revisions.
                match expected_revision {
                    Some(revision) => {
                        self.save_settings_checked(settings, revision).await?;
                    }
                    None => self.save_settings(settings).await?,
                }
                Ok(IpcResponse::Ack)
            }
            IpcRequest::Clear(request) => {
                let cutoff = match request {
                    ClearRequest::All => OffsetDateTime::now_utc(),
                    ClearRequest::OlderThanDays { days } => {
                        OffsetDateTime::now_utc() - time::Duration::days(i64::from(days))
                    }
                };
                self.invalidate_search_cache();
                let deleted = self.store.clear_older_than(cutoff).await?;
                self.invalidate_search_cache();
                self.notify_external_mutation();
                Ok(IpcResponse::Cleared(ClearResponse { deleted }))
            }
            IpcRequest::Doctor => Ok(IpcResponse::Doctor(self.build_doctor_report().await?)),
            IpcRequest::Capabilities => {
                Ok(IpcResponse::Capabilities(Box::new(self.capabilities())))
            }
            IpcRequest::Health => {
                let maintenance = self.maintenance_health.report();
                let capture = self.capture_health.report();
                let ipc = self.ipc_health.report();
                // `ok` flips to false once *either* retention or steady-
                // state capture is wedged so simple health probes (load
                // balancers, oncall checks) light up without needing to
                // inspect the nested struct. IPC handler panics are
                // tracked but do *not* gate `ok`: a one-shot panic on a
                // pathological request is not the same level of
                // degradation as a wedged retention loop, and we'd
                // rather have probes flip on sustained outages than on
                // a single fluke.
                Ok(IpcResponse::Health(HealthResponse {
                    ok: !maintenance.degraded && !capture.degraded,
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    maintenance,
                    capture,
                    ipc,
                }))
            }
            IpcRequest::Shutdown => {
                self.shutdown_handle().cancel();
                Ok(IpcResponse::Ack)
            }
        }
    }

    /// Attach the entry's representation summaries and convert it into the
    /// wire DTO. Shared by every single-entry response (`GetEntry`,
    /// `AddEntry`) so the summary lookup and DTO conversion cannot drift
    /// between request kinds.
    async fn hydrate_entry_dto(
        &self,
        entry: ClipboardEntry,
        include_text: bool,
    ) -> Result<EntryDto> {
        let entry_id = entry.id;
        let summaries = self
            .store
            .list_representation_summaries(&[entry_id])
            .await?;
        let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
        Ok(EntryDto::from_entry(entry, include_text).with_representation_summaries(reps))
    }

    /// Batch-load representation summaries for a list response and hydrate
    /// the rows within the response byte budget (see
    /// [`entry_dtos_within_budget`]). Shared by `ListRecent` / `ListPinned`.
    async fn hydrate_entry_dtos(
        &self,
        entries: Vec<ClipboardEntry>,
        include_sensitive: bool,
    ) -> Result<Vec<EntryDto>> {
        let ids: Vec<_> = entries.iter().map(|e| e.id).collect();
        let summaries = self.store.list_representation_summaries(&ids).await?;
        Ok(entry_dtos_within_budget(
            entries,
            &summaries,
            include_sensitive,
        ))
    }

    pub(crate) async fn build_doctor_report(&self) -> Result<DoctorReport> {
        let settings = self.current_settings();
        let mut permissions = Vec::new();
        // Build the context from the *just-loaded* settings rather than
        // `permission_check()` so the doctor report's permission rows
        // and the rest of the report observe the same settings snapshot.
        // Skipping the side-effecting `permission_check` also avoids
        // racing the first-grant marker write against an in-flight
        // settings update from the desktop shell.
        let ctx = PermissionCheckContext {
            accessibility_prompted_at: settings.onboarding.accessibility_prompted_at,
        };
        if let Some(checker) = &self.permissions
            && let Ok(statuses) = checker.check(&ctx).await
        {
            for status in statuses {
                permissions.push(DoctorPermission {
                    kind: format!("{:?}", status.kind),
                    state: format!("{:?}", status.state),
                    message: status.message,
                });
            }
        }
        let provider_label = match settings.ai.provider {
            AiProviderKind::Disabled => "disabled".to_owned(),
            AiProviderKind::AppleNative => "apple-native".to_owned(),
            AiProviderKind::OpenAiCompatible => "openai-compatible".to_owned(),
        };
        // Best-effort AI availability snapshot. A probe failure (e.g. a Swift
        // bridge error) must not abort the whole report.
        let ai_availability = self.ai_availability().await.ok();
        // Probe the GitHub Releases API for the latest tag so `nagori
        // doctor` can show whether an update is available. Best-effort:
        // the probe runs on every release target (macOS / Windows /
        // Linux all ship a `latest.json` entry today) and is skipped
        // only when the user has disabled background update checks
        // (`auto_update_check`). The probe is rate-limited (24h floor)
        // and hard-disables after consecutive failures so a flapping
        // network can't hammer the GitHub API across repeated doctor
        // calls — see `UpdateProbeState` for the cache + backoff state.
        let latest_version = if settings.auto_update_check {
            self.update_probe.fetch_if_due().await
        } else {
            None
        };
        // Surface thumbnail usage so operators can see whether the LRU
        // budget is doing its job. A read failure here (e.g. corrupt
        // schema in a future migration) must not abort the whole
        // report, so we fall back to `None` and log.
        let thumbnail_total_bytes = match self.store.total_thumbnail_bytes().await {
            Ok(total) => Some(total),
            Err(err) => {
                tracing::warn!(error = %err, "doctor_thumbnail_total_failed");
                None
            }
        };
        Ok(DoctorReport {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            db_path: String::new(),
            socket_path: self.socket_path.display().to_string(),
            capture_enabled: settings.capture_enabled,
            auto_paste_enabled: settings.auto_paste_enabled,
            ai_enabled: settings.ai.enabled,
            auto_update_check: settings.auto_update_check,
            ai_provider: provider_label,
            ai_availability,
            permissions,
            maintenance: self.maintenance_health.report(),
            capture: self.capture_health.report(),
            ipc: self.ipc_health.report(),
            startup: self.startup_health.report(),
            update_channel: settings.update_channel.as_str().to_owned(),
            latest_version,
            thumbnail_total_bytes,
            thumbnail_budget_bytes: settings.max_thumbnail_total_bytes,
        })
    }
}

/// Materialise a list of entries into `EntryDto`s while bounding the peak
/// heap the daemon pays to do so.
///
/// `EntryDto::from_entry` clones each row's full plain text (up to
/// `MAX_ENTRY_SIZE_BYTES` ≈ 768 KiB). A `list_recent` / `list_pinned`
/// response capped at `MAX_READ_LIMIT` (200) rows of large entries could
/// otherwise pull >100 MiB of text into RSS *before* the post-serialize wire
/// guard (`MAX_IPC_RESPONSE_BYTES`) rejects the whole payload — that guard
/// protects the wire and the client's bounded reader, not daemon memory.
///
/// We stop including rows once the cumulative *materialised text* would
/// exceed `MAX_ENTRY_SIZE_BYTES` — the same ceiling a single entry's text is
/// held to, sized so the JSON-escaped payload still fits `MAX_IPC_BYTES`. The
/// client therefore receives a bounded prefix instead of a `response_too_large`
/// rejection, and the peak text allocation is held near one entry's worth
/// rather than scaling with the row count. The first row is always included
/// even when it alone exceeds the budget, so a single large pinned entry never
/// collapses the list to empty. Each candidate's text length is read without
/// cloning, so an over-budget row is never materialised.
fn entry_dtos_within_budget(
    entries: Vec<ClipboardEntry>,
    summaries: &HashMap<EntryId, Vec<RepresentationSummary>>,
    include_sensitive: bool,
) -> Vec<EntryDto> {
    let total = entries.len();
    let mut dtos = Vec::with_capacity(total);
    let mut used: usize = 0;
    for entry in entries {
        let include_text = include_sensitive || is_text_safe_for_default_output(entry.sensitivity);
        let text_len = if include_text {
            entry.plain_text().map_or(0, str::len)
        } else {
            0
        };
        if !dtos.is_empty() && used.saturating_add(text_len) > MAX_ENTRY_SIZE_BYTES {
            tracing::warn!(
                returned = dtos.len(),
                dropped = total - dtos.len(),
                budget_bytes = MAX_ENTRY_SIZE_BYTES,
                "ipc_list_truncated_to_byte_budget"
            );
            break;
        }
        used = used.saturating_add(text_len);
        let entry_id = entry.id;
        let reps = summaries.get(&entry_id).map_or(&[][..], Vec::as_slice);
        dtos.push(EntryDto::from_entry(entry, include_text).with_representation_summaries(reps));
    }
    dtos
}

const fn is_ipc_control_request(request: &IpcRequest) -> bool {
    matches!(
        request,
        IpcRequest::Doctor | IpcRequest::Health | IpcRequest::Capabilities | IpcRequest::Shutdown
    )
}

/// Static, payload-free label for an IPC request, used as the `request_kind`
/// log field. Only the variant is exposed — never the request body — so the
/// dispatch log can never leak clipboard text, queries, or settings.
const fn request_kind(request: &IpcRequest) -> &'static str {
    match request {
        IpcRequest::Search(_) => "search",
        IpcRequest::GetEntry(_) => "get_entry",
        IpcRequest::ListRecent(_) => "list_recent",
        IpcRequest::ListPinned(_) => "list_pinned",
        IpcRequest::AddEntry(_) => "add_entry",
        IpcRequest::CopyEntry(_) => "copy_entry",
        IpcRequest::PasteEntry(_) => "paste_entry",
        IpcRequest::DeleteEntry(_) => "delete_entry",
        IpcRequest::PinEntry(_) => "pin_entry",
        IpcRequest::RunQuickAction(_) => "run_quick_action",
        IpcRequest::RunAiAction(_) => "run_ai_action",
        IpcRequest::GetSettings => "get_settings",
        IpcRequest::UpdateSettings(_) => "update_settings",
        IpcRequest::Clear(_) => "clear",
        IpcRequest::Doctor => "doctor",
        IpcRequest::Capabilities => "capabilities",
        IpcRequest::Health => "health",
        IpcRequest::Shutdown => "shutdown",
    }
}

/// Map a `Result` to the same static outcome label used by the IPC dispatch
/// log so runtime methods can record `result_code` without re-deriving it.
pub(crate) fn result_code<T>(result: &Result<T>) -> &'static str {
    result.as_ref().map_or_else(|err| error_code(err), |_| "ok")
}

pub(crate) const fn error_code(err: &AppError) -> &'static str {
    match err {
        AppError::Storage { .. } => "storage_error",
        AppError::Search { .. } => "search_error",
        AppError::Platform(_) => "platform_error",
        AppError::Permission(_) => "permission_error",
        AppError::Ai(_) => "ai_error",
        AppError::Policy(_) => "policy_error",
        AppError::NotFound => "not_found",
        AppError::InvalidInput(_) => "invalid_input",
        AppError::Unsupported(_) => "unsupported",
        AppError::Configuration(_) => "configuration_error",
        AppError::Conflict(_) => "settings_conflict",
        AppError::Paste { .. } => "paste_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagori_core::EntryFactory;

    fn entry_with_text(len: usize) -> ClipboardEntry {
        // `from_text` tags the row `Sensitivity::Unknown`, which
        // `is_text_safe_for_default_output` admits, so the DTO carries the
        // full text and exercises the cloning path the budget bounds.
        EntryFactory::from_text("a".repeat(len))
    }

    #[test]
    fn keeps_every_row_when_under_budget() {
        let entries = vec![
            entry_with_text(16),
            entry_with_text(16),
            entry_with_text(16),
        ];
        let dtos = entry_dtos_within_budget(entries, &HashMap::new(), false);
        assert_eq!(dtos.len(), 3);
        assert!(dtos.iter().all(|d| d.text.is_some()));
    }

    #[test]
    fn truncates_once_cumulative_text_exceeds_budget() {
        // Three rows at 300 KiB each: 300 + 300 = 600 KiB fits under the
        // 768 KiB ceiling, the third (900 KiB total) does not.
        let chunk = 300 * 1024;
        let entries = vec![
            entry_with_text(chunk),
            entry_with_text(chunk),
            entry_with_text(chunk),
        ];
        let dtos = entry_dtos_within_budget(entries, &HashMap::new(), false);
        assert_eq!(dtos.len(), 2, "the third row must be dropped at the budget");
    }

    #[test]
    fn always_keeps_the_first_row_even_when_oversized() {
        // A single row whose text alone exceeds the budget must still be
        // returned — dropping it would yield a confusing empty list.
        let entries = vec![
            entry_with_text(MAX_ENTRY_SIZE_BYTES + 1024),
            entry_with_text(16),
        ];
        let dtos = entry_dtos_within_budget(entries, &HashMap::new(), false);
        assert_eq!(dtos.len(), 1);
        assert!(dtos[0].text.is_some());
    }

    #[test]
    fn sensitive_rows_cost_nothing_when_text_is_withheld() {
        // When text is withheld (sensitive row, `include_sensitive = false`)
        // the row contributes no text bytes, so a long list of them is not
        // truncated by the text budget.
        let mut entries = Vec::new();
        for _ in 0..8 {
            let mut entry = entry_with_text(300 * 1024);
            entry.sensitivity = nagori_core::Sensitivity::Secret;
            entries.push(entry);
        }
        let dtos = entry_dtos_within_budget(entries, &HashMap::new(), false);
        assert_eq!(
            dtos.len(),
            8,
            "withheld-text rows must not consume the budget"
        );
        assert!(dtos.iter().all(|d| d.text.is_none()));
    }
}

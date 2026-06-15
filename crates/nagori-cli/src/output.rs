use std::path::Path;

use anyhow::Result;
use nagori_core::{
    AppSettings, ClipboardEntry, is_text_safe_for_default_output, safe_preview_for_dto,
};
use nagori_ipc::{AiOutputDto, DoctorReport, EntryDto, SearchResponse, SearchResultDto};
use nagori_platform::{Capability, PlatformCapabilities};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::OutputFormat;

/// Print one JSON record under the shared machine-output contract
/// (docs/cli.md): `--json` renders a pretty multi-line document, `--jsonl`
/// exactly one compact line so line-oriented consumers can split on
/// newlines. Every single-record `print_*` routes its JSON arms through
/// here so no printer can drift back to a pretty dump under `--jsonl`.
/// Callers keep their own `Text` arm; a stray `Text` call still emits the
/// compact single-line form rather than panicking.
pub(crate) fn print_json_record<T: serde::Serialize>(
    value: &T,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(value)?),
        OutputFormat::Jsonl | OutputFormat::Text => println!("{}", serde_json::to_string(value)?),
    }
    Ok(())
}

pub(crate) fn print_entries(
    entries: Vec<ClipboardEntry>,
    format: OutputFormat,
    include_sensitive: bool,
) -> Result<()> {
    let resolve = |entry: &ClipboardEntry| -> bool {
        include_sensitive || is_text_safe_for_default_output(entry.sensitivity)
    };
    match format {
        OutputFormat::Json => {
            let values = entries
                .iter()
                .map(|entry| entry_json(entry, resolve(entry)))
                .collect::<Result<Vec<_>>>()?;
            println!("{}", serde_json::to_string_pretty(&values)?);
        }
        OutputFormat::Jsonl => {
            for entry in &entries {
                println!(
                    "{}",
                    serde_json::to_string(&entry_json(entry, resolve(entry))?)?
                );
            }
        }
        OutputFormat::Text => {
            for entry in entries {
                let kind = entry.content_kind();
                if resolve(&entry) {
                    println!(
                        "{}\t{:?}\t{}",
                        entry.id,
                        kind,
                        entry.plain_text().unwrap_or_default()
                    );
                } else {
                    println!("{}\t{:?}\t{}", entry.id, kind, safe_preview_for_dto(&entry));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn print_entry(
    entry: &ClipboardEntry,
    format: OutputFormat,
    include_text: bool,
) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_record(&entry_json(entry, include_text)?, format)?;
        }
        OutputFormat::Text => {
            if include_text {
                println!("{}", entry.plain_text().unwrap_or_default());
            } else {
                println!(
                    "{}\t{:?}\t{}",
                    entry.id,
                    entry.sensitivity,
                    safe_preview_for_dto(entry)
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn print_search_results(
    results: Vec<nagori_core::SearchResult>,
    format: OutputFormat,
) -> Result<()> {
    let make_value = |result: &nagori_core::SearchResult| -> Result<serde_json::Value> {
        Ok(serde_json::json!({
            "id": result.entry_id,
            "kind": result.content_kind,
            "preview": result.preview,
            "score": result.score,
            "created_at": format_json_time(result.created_at)?,
            "pinned": result.pinned,
            "sensitivity": result.sensitivity,
            "rank_reasons": result.rank_reason,
        }))
    };
    match format {
        OutputFormat::Json => {
            let values = results.iter().map(make_value).collect::<Result<Vec<_>>>()?;
            println!("{}", serde_json::to_string_pretty(&values)?);
        }
        OutputFormat::Jsonl => {
            for result in &results {
                println!("{}", serde_json::to_string(&make_value(result)?)?);
            }
        }
        OutputFormat::Text => {
            for result in results {
                println!(
                    "{}\t{:.1}\t{:?}\t{}",
                    result.entry_id, result.score, result.content_kind, result.preview
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn print_dto_entries(entries: Vec<EntryDto>, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&entries)?),
        OutputFormat::Jsonl => {
            for entry in &entries {
                println!("{}", serde_json::to_string(entry)?);
            }
        }
        OutputFormat::Text => {
            for entry in entries {
                println!("{}\t{:?}\t{}", entry.id, entry.kind, entry.preview);
            }
        }
    }
    Ok(())
}

pub(crate) fn print_dto_entry(entry: &EntryDto, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_record(entry, format)?,
        OutputFormat::Text => {
            if let Some(text) = &entry.text {
                println!("{text}");
            } else {
                println!("{}\t{:?}\t{}", entry.id, entry.sensitivity, entry.preview);
            }
        }
    }
    Ok(())
}

pub(crate) fn print_dto_search(response: SearchResponse, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&response.results)?),
        OutputFormat::Jsonl => {
            for result in &response.results {
                println!("{}", serde_json::to_string(result)?);
            }
        }
        OutputFormat::Text => {
            for result in response.results {
                print_dto_search_row(&result);
            }
        }
    }
    Ok(())
}

pub(crate) fn print_clear_result(deleted: usize, format: OutputFormat) -> Result<()> {
    match format {
        // Route the JSON arms through the shared printer so `--json` renders
        // the documented pretty document (not a compact line). The previous
        // `println!(json!(…))` rendered compact under both `--json` and
        // `--jsonl`, contradicting the single-record contract.
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_record(&serde_json::json!({ "deleted": deleted }), format)
        }
        OutputFormat::Text => {
            println!("deleted {deleted}");
            Ok(())
        }
    }
}

/// Replace the user's home prefix on `path` with `~` when rendering for
/// human consumption. `nagori doctor` prints DB / socket / token paths
/// to stdout, which routinely shows up in shared terminals, paired
/// programming sessions, and screenshots posted to issue trackers — and
/// the absolute path is just the username with extra steps. The JSON /
/// JSONL paths still emit the full value untouched so automation can
/// parse them without re-expanding `~`.
pub(crate) fn shorten_home(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return if rel.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rel.display())
        };
    }
    path.display().to_string()
}

pub(crate) fn print_doctor_report(report: &DoctorReport, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_record(report, format)?;
        }
        OutputFormat::Text => {
            println!("version\t{}", report.version);
            let latest = report.latest_version.as_deref().unwrap_or("(unknown)");
            println!("version_latest\t{latest}");
            let channel = if report.update_channel.is_empty() {
                "stable"
            } else {
                report.update_channel.as_str()
            };
            println!("update_channel\t{channel}");
            println!("socket\t{}", shorten_home(Path::new(&report.socket_path)));
            if !report.db_path.is_empty() {
                println!("db\t{}", shorten_home(Path::new(&report.db_path)));
            }
            println!("capture_enabled\t{}", report.capture_enabled);
            println!("auto_paste_enabled\t{}", report.auto_paste_enabled);
            println!("ai_enabled\t{}", report.ai_enabled);
            // The only background network call the daemon makes is the
            // updater probe; surface its enablement so operators see at
            // a glance whether anything is allowed to reach the network.
            println!("auto_update_check\t{}", report.auto_update_check);
            println!("ai_provider\t{}", report.ai_provider);
            for permission in &report.permissions {
                let suffix = permission
                    .message
                    .as_deref()
                    .map_or_else(String::new, |msg| format!("\t{msg}"));
                println!(
                    "permission\t{}\t{}{}",
                    permission.kind, permission.state, suffix
                );
            }
            let maintenance = &report.maintenance;
            let state = if maintenance.degraded {
                "degraded"
            } else {
                "ok"
            };
            let suffix = maintenance
                .last_error
                .as_deref()
                .map_or_else(String::new, |msg| format!("\t{msg}"));
            println!(
                "maintenance\t{state}\tconsecutive_failures={}{suffix}",
                maintenance.consecutive_failures
            );
            // Steady-state capture-loop health. `degraded` flips once
            // the per-tick failure counter crosses the daemon's threshold;
            // the category column distinguishes "we lost visibility"
            // (`adapter` / `settings_load` / `storage`) from "we're
            // rejecting on purpose" (`policy` / `oversized_drop`) so an
            // operator can tell a permission outage apart from a
            // too-tight denylist or a wedged disk.
            let capture = &report.capture;
            let capture_state = if capture.degraded { "degraded" } else { "ok" };
            let capture_category = capture
                .last_event_category
                .map_or("none", format_capture_event_category);
            let capture_suffix = capture
                .last_error
                .as_deref()
                .map_or_else(String::new, |msg| format!("\t{msg}"));
            println!(
                "capture\t{capture_state}\tconsecutive_failures={}\tlast_event={capture_category}{capture_suffix}",
                capture.consecutive_failures
            );
            // Capture loop's pre-poll init status. `ready=true` means the
            // host process loaded settings and entered polling; a
            // recorded `last_error` is the silent-abort case the desktop
            // notification gate also branches on.
            let startup = &report.startup;
            let startup_state = match (startup.ready, startup.last_error.as_deref()) {
                (true, _) => "ready",
                (false, Some(_)) => "failed",
                (false, None) => "pending",
            };
            let startup_suffix = startup
                .last_error
                .as_deref()
                .map_or_else(String::new, |msg| format!("\t{msg}"));
            println!("startup\t{startup_state}{startup_suffix}");
            // Thumbnail cache footprint and configured budget. `used`
            // is the current `entry_thumbnails` total; `cap` reflects
            // `max_thumbnail_total_bytes`. Operators check this when
            // diagnosing "preview is slow on a big image" — a near-cap
            // total combined with frequent regeneration suggests
            // raising the budget.
            let thumb_used = report
                .thumbnail_total_bytes
                .map_or_else(|| "(unknown)".to_owned(), |b| b.to_string());
            let thumb_cap = report
                .thumbnail_budget_bytes
                .map_or_else(|| "disabled".to_owned(), |b| b.to_string());
            println!("thumbnails\tused={thumb_used}\tcap={thumb_cap}");
            // IPC accept-loop health: per-process panic counter so an
            // operator can correlate "auto-paste sometimes stalls" with
            // a handler that crashed and got silently restarted by the
            // JoinSet. A non-zero count surfaces the last panic message
            // for one-glance triage; the top-level `ok` flag is not
            // flipped because a one-shot panic is not the same class of
            // failure as a wedged retention loop.
            let ipc = &report.ipc;
            let ipc_suffix = ipc
                .last_panic_message
                .as_deref()
                .map_or_else(String::new, |msg| format!("\t{msg}"));
            // `max_concurrent_connections == 0` means the daemon predates
            // the config-on-wire change; show `(unknown)` rather than `0`
            // so operators don't read it as "the daemon refuses every
            // handler".
            let max_conns = if ipc.max_concurrent_connections == 0 {
                "(unknown)".to_owned()
            } else {
                ipc.max_concurrent_connections.to_string()
            };
            println!(
                "ipc\tpanic_count={}\tpanics_last_5m={}\tmax_connections={max_conns}{ipc_suffix}",
                ipc.handler_panic_count, ipc.panics_last_5m,
            );
        }
    }
    Ok(())
}

/// Render a `CaptureEventCategory` as the stable `snake_case` token used
/// in `nagori doctor` output. Kept separate from the enum's `Display`
/// (which `serde` already provides via `rename_all = "snake_case"`) so
/// the text formatter doesn't reach through `serde_json` for every
/// doctor row.
pub(crate) const fn format_capture_event_category(
    category: nagori_ipc::CaptureEventCategory,
) -> &'static str {
    use nagori_ipc::CaptureEventCategory;
    match category {
        CaptureEventCategory::SettingsLoad => "settings_load",
        CaptureEventCategory::Adapter => "adapter",
        CaptureEventCategory::Storage => "storage",
        CaptureEventCategory::Policy => "policy",
        CaptureEventCategory::OversizedDrop => "oversized_drop",
    }
}

pub(crate) fn print_capabilities(caps: &PlatformCapabilities, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_record(caps, format)?,
        OutputFormat::Text => {
            // Tab-separated to match `nagori doctor`. Every row has the
            // form `<field>\t<status>[\tdetail…]`; downstream scripts
            // can split on the first tab and not lose extra detail.
            println!("platform\t{:?}", caps.platform);
            println!("tier\t{:?}", caps.tier);
            print_capability_row("capture_text", &caps.capture_text);
            print_capability_row("capture_image", &caps.capture_image);
            print_capability_row("capture_files", &caps.capture_files);
            print_capability_row("write_text", &caps.write_text);
            print_capability_row("write_image", &caps.write_image);
            print_capability_row("auto_paste", &caps.auto_paste);
            print_capability_row("global_hotkey", &caps.global_hotkey);
            print_capability_row("frontmost_app", &caps.frontmost_app);
            print_capability_row("permissions_ui", &caps.permissions_ui);
            print_capability_row("update_check", &caps.update_check);
        }
    }
    Ok(())
}

pub(crate) fn print_capability_row(field: &str, cap: &Capability) {
    match cap {
        Capability::Available => println!("{field}\tavailable"),
        Capability::Experimental { message } => {
            println!("{field}\texperimental\t{message}");
        }
        Capability::Unsupported { reason } => {
            println!("{field}\tunsupported\t{reason}");
        }
        Capability::RequiresPermission {
            permission,
            message,
        } => {
            println!("{field}\trequires_permission\t{permission:?}\t{message}");
        }
        Capability::RequiresExternalTool { tool, install_hint } => {
            let hint = install_hint.as_deref().unwrap_or("");
            println!("{field}\trequires_external_tool\t{tool}\t{hint}");
        }
    }
}

pub(crate) fn print_dto_search_row(result: &SearchResultDto) {
    println!(
        "{}\t{:.1}\t{:?}\t{}",
        result.id, result.score, result.kind, result.preview
    );
}

pub(crate) fn entry_json(entry: &ClipboardEntry, include_text: bool) -> Result<serde_json::Value> {
    let text = include_text.then(|| entry.plain_text().unwrap_or_default().to_owned());
    Ok(serde_json::json!({
        "id": entry.id,
        "kind": entry.content_kind(),
        "text": text,
        "preview": safe_preview_for_dto(entry),
        "created_at": format_json_time(entry.metadata.created_at)?,
        "updated_at": format_json_time(entry.metadata.updated_at)?,
        "last_used_at": entry.metadata.last_used_at.map(format_json_time).transpose()?,
        "use_count": entry.metadata.use_count,
        "pinned": entry.lifecycle.pinned,
        "sensitivity": entry.sensitivity,
    }))
}

pub(crate) fn format_json_time(value: OffsetDateTime) -> Result<String> {
    value.format(&Rfc3339).map_err(Into::into)
}

pub(crate) fn print_ack(format: OutputFormat) -> Result<()> {
    match format {
        // See `print_clear_result`: the JSON arms go through the shared
        // printer so `--json` is pretty-printed like every other record.
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_record(&serde_json::json!({ "ok": true }), format)
        }
        OutputFormat::Text => {
            println!("ok");
            Ok(())
        }
    }
}

pub(crate) fn print_ai_output(output: &AiOutputDto, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_record(output, format)?,
        OutputFormat::Text => {
            println!("{}", output.text);
            for warning in &output.warnings {
                eprintln!("warning: {warning}");
            }
        }
    }
    Ok(())
}

/// `nagori daemon status` local arm. This path inspects the `SQLite` store
/// directly and never contacts a daemon, so the output names its source
/// (`local`) instead of claiming `ok` — an earlier version printed `ok`,
/// which read as "the daemon is healthy" even when nothing was running.
/// Probing the daemon requires the `--ipc` / `--auto-ipc` routes.
pub(crate) fn print_status(
    db_path: &Path,
    settings: &AppSettings,
    format: OutputFormat,
) -> Result<()> {
    if format.is_json() {
        print_json_record(
            &serde_json::json!({
                "source": "local",
                "daemon_probed": false,
                "db": db_path,
                "capture_enabled": settings.capture_enabled,
                "ai_enabled": settings.ai.enabled,
                "auto_paste_enabled": settings.auto_paste_enabled,
                "history_retention_count": settings.history_retention_count,
            }),
            format,
        )?;
    } else {
        // Collapse the home prefix to `~` for the human row, matching
        // `nagori doctor`; the JSON arm above keeps the full path for
        // automation.
        println!("local (daemon not probed)\t{}", shorten_home(db_path));
    }
    Ok(())
}

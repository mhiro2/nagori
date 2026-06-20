use std::path::Path;

use anyhow::Result;
use nagori_core::SettingsRepository;
use nagori_ipc::{DoctorPermission, DoctorReport, IpcRequest};
use nagori_storage::SqliteStore;

use super::{Executor, expect_doctor};
use crate::OutputFormat;
use crate::output::{print_doctor_report, shorten_home};

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use nagori_platform::{PermissionCheckContext, PermissionChecker};
#[cfg(target_os = "linux")]
use nagori_platform_linux::LinuxPermissionChecker;
#[cfg(target_os = "macos")]
use nagori_platform_macos::MacosPermissionChecker;
#[cfg(target_os = "windows")]
use nagori_platform_windows::WindowsPermissionChecker;

pub async fn run(executor: &Executor, format: OutputFormat) -> Result<()> {
    match executor {
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            print_local_doctor(&ctx.db_path, &store, format).await
        }
        Executor::Ipc(ctx) => {
            let resp = ctx.client.send(IpcRequest::Doctor).await?;
            print_doctor_report(&expect_doctor(resp)?, format)
        }
    }
}

/// Stable label for the configured AI provider family.
const fn ai_provider_label(provider: nagori_core::AiProviderKind) -> &'static str {
    match provider {
        nagori_core::AiProviderKind::Disabled => "disabled",
        nagori_core::AiProviderKind::AppleNative => "apple-native",
        nagori_core::AiProviderKind::OpenAiCompatible => "openai-compatible",
    }
}

/// Run the host permission checker and collect its rows so the text and
/// JSON arms render the same probe results. A probe failure degrades to
/// "no rows" rather than aborting the report, matching the daemon's
/// best-effort doctor behaviour.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
async fn collect_permission_statuses(
    ctx: &PermissionCheckContext,
) -> Vec<nagori_platform::PermissionStatus> {
    #[cfg(target_os = "macos")]
    let checker = MacosPermissionChecker;
    #[cfg(target_os = "windows")]
    let checker = WindowsPermissionChecker;
    #[cfg(target_os = "linux")]
    let checker = LinuxPermissionChecker;
    checker.check(ctx).await.unwrap_or_default()
}

async fn print_local_doctor(
    db_path: &Path,
    store: &SqliteStore,
    format: OutputFormat,
) -> Result<()> {
    let settings = store.get_settings().await?;
    // The macOS checker keys NotDetermined vs Denied off this timestamp;
    // build the context once and share it across the per-OS branches.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    let statuses = collect_permission_statuses(&PermissionCheckContext {
        accessibility_prompted_at: settings.onboarding.accessibility_prompted_at,
    })
    .await;
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let statuses: Vec<nagori_platform::PermissionStatus> = Vec::new();
    let thumbnail_total_bytes = store.total_thumbnail_bytes().await.ok();
    let data_dir_sync_warning = data_dir_sync_warning(db_path);

    if format.is_json() {
        // Build a real `DoctorReport` so the local arm's JSON is the same
        // schema as the daemon's — a consumer must be able to deserialize
        // either arm's output into one type without branching on which
        // transport happened to serve the report. Daemon-only sections
        // (maintenance / capture / IPC / startup health) carry their serde
        // defaults — exactly what a legacy-daemon report deserializes to —
        // and `socket_path` is empty because no endpoint was contacted;
        // `latest_version` stays `None` because the local arm never probes
        // the network.
        let report = DoctorReport {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            db_path: db_path.display().to_string(),
            socket_path: String::new(),
            capture_enabled: settings.capture_enabled,
            auto_paste_enabled: settings.auto_paste_enabled,
            ai_enabled: settings.ai.enabled,
            auto_update_check: settings.auto_update_check,
            ai_provider: ai_provider_label(settings.ai.provider).to_owned(),
            ai_availability: None,
            permissions: statuses
                .iter()
                .map(|status| DoctorPermission {
                    kind: format!("{:?}", status.kind),
                    state: format!("{:?}", status.state),
                    message: status.message.clone(),
                })
                .collect(),
            maintenance: nagori_ipc::MaintenanceHealthReport::default(),
            capture: nagori_ipc::CaptureHealthReport::default(),
            ipc: nagori_ipc::IpcHealthReport::default(),
            startup: nagori_ipc::StartupHealthReport::default(),
            update_channel: settings.update_channel.as_str().to_owned(),
            latest_version: None,
            thumbnail_total_bytes,
            thumbnail_budget_bytes: settings.max_thumbnail_total_bytes,
            data_dir_sync_warning,
        };
        return print_doctor_report(&report, format);
    }

    println!("version\t{}", env!("CARGO_PKG_VERSION"));
    println!("version_latest\t(unknown)");
    println!("update_channel\t{}", settings.update_channel.as_str());
    println!("db\t{}", shorten_home(db_path));
    println!("capture_enabled\t{}", settings.capture_enabled);
    println!("auto_paste_enabled\t{}", settings.auto_paste_enabled);
    println!("ai_enabled\t{}", settings.ai.enabled);
    println!("auto_update_check\t{}", settings.auto_update_check);
    println!("ai_provider\t{}", ai_provider_label(settings.ai.provider));
    for status in &statuses {
        let suffix = status
            .message
            .as_deref()
            .map_or_else(String::new, |msg| format!("\t{msg}"));
        println!(
            "permission\t{:?}\t{:?}{}",
            status.kind, status.state, suffix
        );
    }
    let thumb_used =
        thumbnail_total_bytes.map_or_else(|| "(unknown)".to_owned(), |b| b.to_string());
    let thumb_cap = settings
        .max_thumbnail_total_bytes
        .map_or_else(|| "disabled".to_owned(), |b| b.to_string());
    println!("thumbnails\tused={thumb_used}\tcap={thumb_cap}");
    if let Some(warning) = &data_dir_sync_warning {
        println!("data_dir_sync_warning\t{warning}");
    }
    Ok(())
}

/// Warn when the data directory lives inside a cloud-sync folder, which
/// would copy the plaintext clipboard history off-device. Computed from
/// the DB file's parent directory; `None` when it is not under a known
/// sync root (see `nagori_core::storage_location`).
fn data_dir_sync_warning(db_path: &Path) -> Option<String> {
    let data_dir = db_path.parent().unwrap_or(db_path);
    nagori_core::detect_cloud_sync(data_dir).map(|m| m.describe())
}

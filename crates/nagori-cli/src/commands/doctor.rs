use std::path::Path;

use anyhow::Result;
use nagori_core::SettingsRepository;
use nagori_ipc::IpcRequest;
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
            print_local_doctor(&ctx.db_path, &store).await
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

async fn print_local_doctor(db_path: &Path, store: &SqliteStore) -> Result<()> {
    let settings = store.get_settings().await?;
    println!("version\t{}", env!("CARGO_PKG_VERSION"));
    println!("version_latest\t(unknown)");
    println!("update_channel\t{}", settings.update_channel.as_str());
    println!("db\t{}", shorten_home(db_path));
    println!("capture_enabled\t{}", settings.capture_enabled);
    println!("auto_paste_enabled\t{}", settings.auto_paste_enabled);
    println!("ai_enabled\t{}", settings.ai.enabled);
    println!("auto_update_check\t{}", settings.auto_update_check);
    println!("ai_provider\t{}", ai_provider_label(settings.ai.provider));
    // The macOS checker keys NotDetermined vs Denied off this timestamp;
    // build the context once and share it across the per-OS branches.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    let permission_ctx = PermissionCheckContext {
        accessibility_prompted_at: settings.onboarding.accessibility_prompted_at,
    };
    #[cfg(target_os = "macos")]
    {
        let checker = MacosPermissionChecker;
        if let Ok(statuses) = checker.check(&permission_ctx).await {
            for status in statuses {
                let suffix = status
                    .message
                    .as_deref()
                    .map_or_else(String::new, |msg| format!("\t{msg}"));
                println!(
                    "permission\t{:?}\t{:?}{}",
                    status.kind, status.state, suffix
                );
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let checker = WindowsPermissionChecker;
        if let Ok(statuses) = checker.check(&permission_ctx).await {
            for status in statuses {
                let suffix = status
                    .message
                    .as_deref()
                    .map_or_else(String::new, |msg| format!("\t{msg}"));
                println!(
                    "permission\t{:?}\t{:?}{}",
                    status.kind, status.state, suffix
                );
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let checker = LinuxPermissionChecker;
        if let Ok(statuses) = checker.check(&permission_ctx).await {
            for status in statuses {
                let suffix = status
                    .message
                    .as_deref()
                    .map_or_else(String::new, |msg| format!("\t{msg}"));
                println!(
                    "permission\t{:?}\t{:?}{}",
                    status.kind, status.state, suffix
                );
            }
        }
    }
    let thumb_used = store
        .total_thumbnail_bytes()
        .await
        .map_or_else(|_| "(unknown)".to_owned(), |b| b.to_string());
    let thumb_cap = settings
        .max_thumbnail_total_bytes
        .map_or_else(|| "disabled".to_owned(), |b| b.to_string());
    println!("thumbnails\tused={thumb_used}\tcap={thumb_cap}");
    Ok(())
}

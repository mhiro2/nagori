use std::path::PathBuf;

use anyhow::{Context as _, Result};
use nagori_core::SettingsRepository;
use nagori_daemon::{CliIpcConfig, DaemonConfig, default_socket_path};
use nagori_ipc::{IpcRequest, IpcResponse};
use nagori_storage::SqliteStore;

use super::{Executor, expect_ack};
use crate::output::{print_ack, print_json_record, print_status};
use crate::{DaemonRunArgs, OutputFormat, default_db_path};

pub async fn status(executor: &Executor, format: OutputFormat) -> Result<()> {
    match executor {
        Executor::Local(ctx) => {
            let store = ctx.open_store()?;
            let settings = store.get_settings().await?;
            print_status(&ctx.db_path, &settings, format)
        }
        Executor::Ipc(ctx) => {
            let resp = ctx.client.send(IpcRequest::Health).await?;
            let IpcResponse::Health(health) = resp else {
                anyhow::bail!("unexpected ipc response");
            };
            if format.is_json() {
                print_json_record(
                    &serde_json::json!({
                        "source": "daemon",
                        "ok": health.ok,
                        "version": health.version,
                    }),
                    format,
                )?;
            } else {
                println!("ok\t{}", health.version);
            }
            Ok(())
        }
    }
}

pub async fn stop(executor: &Executor, format: OutputFormat) -> Result<()> {
    match executor {
        // Open the store before bailing — the pre-split dispatcher's
        // precedence, so a broken `--db` keeps surfacing as the open error.
        Executor::Local(ctx) => {
            let _store = ctx.open_store()?;
            anyhow::bail!("daemon stop requires --ipc <socket>")
        }
        Executor::Ipc(ctx) => {
            expect_ack(ctx.client.send(IpcRequest::Shutdown).await?)?;
            print_ack(format);
            Ok(())
        }
    }
}

/// Treat an env var as a boolean opt-in. Only an explicit truthy token
/// (`1` / `true` / `yes` / `on`, case-insensitive) flips the flag on; anything
/// else — unset, empty, `0`, `false`, `no`, garbage — keeps it off. Used for
/// security-relaxation flags where silently honouring `=0` would be a footgun.
fn env_truthy(name: &str) -> bool {
    std::env::var(name).is_ok_and(|raw| {
        matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// `nagori daemon run`: bootstrap the store + native adapters and serve
/// until shutdown. Routed before the executor decision in `dispatch` —
/// the daemon *is* the IPC server, so neither executor arm applies.
// On platforms without a native adapter the body short-circuits via
// `bail!`, so no `.await` runs.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows")),
    allow(clippy::unused_async)
)]
pub async fn run_server(
    db: Option<PathBuf>,
    ipc: Option<PathBuf>,
    args: DaemonRunArgs,
) -> Result<()> {
    let db_path = db.unwrap_or_else(default_db_path);
    let data_dir = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(
            || std::path::PathBuf::from("."),
            std::path::Path::to_path_buf,
        );
    nagori_storage::ensure_private_directory(&data_dir)
        .with_context(|| format!("failed to create {}", data_dir.display()))?;
    // Single-instance gate: take the data-directory lock before opening the
    // store, so a second daemon (or the desktop app, which locks the same
    // directory) never runs migrations or a capture loop against a DB this
    // process is about to own. Held until `run_daemon` returns.
    let instance_lock = nagori_daemon::acquire_data_dir_lock(&data_dir)
        .context("refusing to start a second daemon")?;
    let store = SqliteStore::open(&db_path)
        .with_context(|| format!("failed to open {}", db_path.display()))?;

    let socket_path = ipc.unwrap_or_else(default_socket_path);
    // Test harnesses (notably scripts/e2e-macos.sh) cannot grant the daemon
    // Accessibility permission programmatically, so AX queries fail every
    // tick and the capture loop's "after N AX errors, treat focus as
    // secure" escalation drops user-issued pbcopy events. Letting the
    // harness opt out via env var keeps production safety intact (default
    // remains fail-closed) while making the e2e pipeline exercisable.
    //
    // Only accept explicit truthy values: anything else — including the
    // common footgun `=0`, `=false`, or `=no` — leaves fail-closed on. A
    // security-relaxation flag should not be enabled by accident.
    let secure_focus_fail_closed = !env_truthy("NAGORI_DISABLE_SECURE_FOCUS_FAIL_CLOSED");
    // Pair the token file with the IPC endpoint so a daemon launched with
    // `--ipc <custom>` doesn't trample the default daemon's token file
    // (and vice versa). The CLI's IPC client mirrors this derivation so
    // client and daemon agree on the path.
    let token_path = nagori_ipc::token_path_for_endpoint(&socket_path)
        .context("failed to resolve the IPC auth token path")?;
    let ipc_defaults = CliIpcConfig::default();
    let max_concurrent_connections = args
        .ipc_max_connections
        .unwrap_or(ipc_defaults.max_concurrent_connections);
    let config = DaemonConfig {
        ipc: CliIpcConfig {
            socket_path,
            token_path,
            max_concurrent_connections,
            ..ipc_defaults
        },
        capture_interval: std::time::Duration::from_millis(args.capture_interval_ms),
        // The clap range above already keeps this well clear of overflow;
        // `saturating_mul` is belt-and-suspenders in case the bound is ever
        // relaxed without revisiting this arithmetic.
        maintenance_interval: std::time::Duration::from_secs(
            args.maintenance_interval_min.saturating_mul(60),
        ),
        secure_focus_fail_closed,
    };

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        use nagori_platform_native::{NativeRuntimeOptions, build_native_runtime};

        let parts = build_native_runtime(
            store,
            NativeRuntimeOptions {
                socket_path: Some(config.ipc.socket_path.clone()),
                ai_engine: None,
            },
        )?;
        nagori_daemon::run_daemon(
            parts.runtime,
            parts.clipboard_reader,
            config,
            Some(parts.window),
            instance_lock,
        )
        .await?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (store, config, instance_lock);
        anyhow::bail!("daemon run is only available on macOS, Windows, and Linux in this build")
    }
}

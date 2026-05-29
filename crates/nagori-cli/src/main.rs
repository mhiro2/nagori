use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use futures::StreamExt;
use nagori_core::{
    AiActionId, AiEvent, AiRequestOptions, AppError, EntryId, EntryRepository, QuickActionId,
    SearchQuery, SettingsRepository, is_text_safe_for_default_output,
};
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use nagori_daemon::run_daemon;
use nagori_daemon::{DaemonConfig, NagoriRuntime, default_socket_path};
use nagori_ipc::{
    AddEntryRequest, AiOutputDto, ClearRequest, ClearResponse, CopyEntryRequest,
    DeleteEntryRequest, DoctorReport, EntryDto, GetEntryRequest, IpcClient, IpcRequest,
    IpcResponse, ListPinnedRequest, ListRecentRequest, PasteEntryRequest, PinEntryRequest,
    RunAiActionRequest, RunQuickActionRequest, SearchRequest, SearchResponse,
};
use nagori_platform::{MemoryClipboard, NoopPasteController, PlatformCapabilities};
use nagori_platform_native::{NativeRuntimeOptions, build_native_runtime};
use nagori_search::normalize_text;
use nagori_storage::SqliteStore;
use time::OffsetDateTime;

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use nagori_platform::{PermissionCheckContext, PermissionChecker};
#[cfg(target_os = "linux")]
use nagori_platform_linux::LinuxPermissionChecker;
#[cfg(target_os = "macos")]
use nagori_platform_macos::MacosPermissionChecker;
#[cfg(target_os = "windows")]
use nagori_platform_windows::WindowsPermissionChecker;

mod output;

use output::{
    print_ack, print_ai_output, print_capabilities, print_clear_result, print_doctor_report,
    print_dto_entries, print_dto_entry, print_dto_search, print_entries, print_entry,
    print_search_results, print_status, shorten_home,
};

#[derive(Debug, Parser)]
#[command(name = "nagori")]
#[command(about = "Local-first clipboard history CLI")]
struct Cli {
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Path to the daemon socket. When omitted, the CLI uses the local DB
    /// directly unless `--auto-ipc` is set, which auto-connects to the
    /// default socket if the daemon is reachable.
    #[arg(long, global = true)]
    ipc: Option<PathBuf>,
    /// Try the default socket; fall back to direct DB access if unreachable.
    #[arg(long, global = true)]
    auto_ipc: bool,
    /// Pretty JSON output (single payload).
    #[arg(long, global = true)]
    json: bool,
    /// JSON Lines output (one record per line). Conflicts with --json.
    #[arg(long, global = true, conflicts_with = "json")]
    jsonl: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    List(ListArgs),
    Search(SearchArgs),
    Get(GetArgs),
    Add(AddArgs),
    Delete(IdArgs),
    Pin(IdArgs),
    Unpin(IdArgs),
    Copy(IdArgs),
    Paste(IdArgs),
    Clear(ClearArgs),
    /// Run a deterministic on-device quick action against an entry.
    Quick(QuickArgs),
    /// Run a model-backed AI action against an entry (streams by default).
    Ai(AiArgs),
    Doctor,
    /// Print the host adapter's capability matrix (clipboard / paste /
    /// hotkey / etc.) — what nagori can do on this OS *given the right
    /// permissions and tools*. Pair with `nagori doctor` to see the
    /// live permission/tool state.
    Capabilities,
    Daemon(DaemonArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    pinned: bool,
    /// Include full text for Private/Secret entries.
    #[arg(long)]
    include_sensitive: bool,
}

#[derive(Debug, Args)]
struct SearchArgs {
    query: String,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Debug, Args)]
struct GetArgs {
    id: String,
    #[arg(long)]
    include_sensitive: bool,
}

#[derive(Debug, Args)]
struct AddArgs {
    #[arg(long, conflicts_with = "stdin")]
    text: Option<String>,
    #[arg(long)]
    stdin: bool,
}

#[derive(Debug, Args)]
struct IdArgs {
    id: String,
}

#[derive(Debug, Args)]
#[command(group = clap::ArgGroup::new("clear_scope").required(true).args(&["older_than_days", "all"]))]
struct ClearArgs {
    #[arg(long)]
    older_than_days: Option<i64>,
    /// Wipe every unpinned entry. Required when no time window is given.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
struct QuickArgs {
    action: QuickActionId,
    id: String,
}

#[derive(Debug, Args)]
struct AiArgs {
    action: AiActionId,
    id: String,
    /// Target language for `translate` (BCP-47 / ISO code, e.g. `ja`, `en`,
    /// `zh-Hans`). Required for `translate`; ignored by other actions.
    #[arg(long)]
    to: Option<String>,
    /// Source language for `translate`; auto-detected from the input when
    /// omitted.
    #[arg(long)]
    from: Option<String>,
    /// Print only the final result instead of streaming as it is generated.
    /// Streaming (the default) emits JSON Lines under `--json`/`--jsonl`, or
    /// plain text to stdout otherwise.
    #[arg(long)]
    no_stream: bool,
}

#[derive(Debug, Args)]
struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    Status,
    Run(DaemonRunArgs),
    Stop,
}

#[derive(Debug, Args)]
struct DaemonRunArgs {
    /// Clipboard poll interval. Must be non-zero — `0` would spin the
    /// capture loop into a busy loop — and is capped at one hour so a
    /// fat-fingered value can't silently disable capture for days.
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(1..=3_600_000))]
    capture_interval_ms: u64,
    /// Maintenance sweep interval in minutes. Non-zero (a `0` interval
    /// busy-loops the maintenance task) and capped well below the point
    /// where `* 60` would overflow the seconds `Duration`.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=525_600))]
    maintenance_interval_min: u64,
    /// Cap on concurrent IPC handlers. Defaults to the IPC crate's
    /// built-in ceiling; tune down in regression tests or up when the
    /// daemon serves many automated probes simultaneously. Must be
    /// non-zero — `0` is rejected at parse time.
    #[arg(long)]
    ipc_max_connections: Option<NonZeroUsize>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(exit_code_for(&err))
        }
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    if matches!(
        cli.command,
        Command::Daemon(DaemonArgs {
            command: DaemonCommand::Run(_)
        })
    ) {
        init_tracing();
        return run_daemon_command(cli).await;
    }
    if let Some(socket) = cli.ipc.clone() {
        return run_ipc_command(cli, socket).await;
    }
    // `--db <path>` is an explicit direct-DB request; honor it as-is so the
    // user can still poke at an offline DB even with a daemon running.
    if cli.db.is_none() {
        let writes = is_write_command(&cli.command);
        // Writes default to IPC so the daemon stays the single source of
        // truth for capture / settings / clipboard state. Reads only try
        // IPC under explicit `--auto-ipc`, preserving the existing
        // "read straight from disk" UX for casual queries.
        if writes || cli.auto_ipc {
            let candidate = default_socket_path();
            if let Ok(token) =
                nagori_ipc::read_token_file(&nagori_ipc::token_path_for_endpoint(&candidate))
                && IpcClient::new(candidate.to_string_lossy().as_ref(), token)
                    .send(IpcRequest::Health)
                    .await
                    .is_ok()
            {
                return run_ipc_command(cli, candidate).await;
            }
            if writes {
                anyhow::bail!(
                    "write commands require a running daemon. Run `nagori daemon run` \
                     or pass an explicit `--db <path>` to operate on a local DB."
                );
            }
            // `--auto-ipc` was set but the daemon either had no readable
            // token or failed the health probe. Reads silently fall back
            // to opening the SQLite file directly, which is documented in
            // `docs/cli.md` but easy to miss — and in this mode any writes
            // the daemon makes after our snapshot won't reach us, and any
            // local cache invalidation we'd normally trigger via IPC isn't
            // delivered to the running daemon. Surface the fallback at
            // warn! (stderr) so a user debugging stale results sees the
            // mismatch instead of having to bisect why their query lags.
            init_tracing();
            tracing::warn!(
                socket = %candidate.display(),
                "ipc_fallback_to_local_db reason=daemon_unreachable mode=local-fallback"
            );
        }
    }
    run_local_command(cli).await
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

const fn is_write_command(cmd: &Command) -> bool {
    // `Quick` / `Ai` only read the target entry and never mutate the store, so
    // they're not write commands — and `Ai` in particular runs its engine
    // in-process so it can stream, rather than routing to the daemon.
    matches!(
        cmd,
        Command::Add(_)
            | Command::Delete(_)
            | Command::Pin(_)
            | Command::Unpin(_)
            | Command::Copy(_)
            | Command::Paste(_)
            | Command::Clear(_)
    )
}

fn exit_code_for(err: &anyhow::Error) -> u8 {
    if let Some(app) = err.downcast_ref::<AppError>() {
        return match app {
            AppError::NotFound => 4,
            AppError::Policy(_) => 5,
            AppError::InvalidInput(_) => 2,
            AppError::Permission(_) => 6,
            AppError::Unsupported(_) => 7,
            AppError::Storage(_)
            | AppError::Search(_)
            | AppError::Platform(_)
            | AppError::Ai(_)
            | AppError::Configuration(_) => 8,
        };
    }
    // No `AppError` reached us. The previous behaviour substring-matched
    // the rendered message (`"not found"`, `"policy"`, `"invalid"`) for
    // a best-effort classification, but that drifted as soon as anyhow
    // contextualised the chain — `with_context("failed to open …")`
    // hid `"NotFound"` underneath the wrapper, dumping us into the
    // generic 1 bucket. Anything that hasn't already been promoted into
    // `AppError` (IPC-level translations now go through
    // `ipc_error_to_anyhow`) is by definition an internal failure: a
    // serialiser bug, an unexpected IPC variant, missing config files,
    // etc. Map those to 8 (internal error) so the exit code is stable
    // regardless of how the message string evolves.
    8
}

/// Translate an IPC-level error response into an `anyhow::Error` whose
/// root cause is the structured `AppError`. Without this, the CLI's
/// `exit_code_for` would only see the rendered `"<code>: <message>"`
/// string and fall through to the internal-error bucket.
fn ipc_error_to_anyhow(err: &nagori_ipc::IpcError) -> anyhow::Error {
    let app = match err.code.as_str() {
        "not_found" => AppError::NotFound,
        "invalid_input" => AppError::InvalidInput(err.message.clone()),
        "policy_error" => AppError::Policy(err.message.clone()),
        "permission_error" => AppError::Permission(err.message.clone()),
        "unsupported" => AppError::Unsupported(err.message.clone()),
        "storage_error" => AppError::Storage(err.message.clone()),
        "search_error" => AppError::Search(err.message.clone()),
        "platform_error" => AppError::Platform(err.message.clone()),
        "ai_error" => AppError::Ai(err.message.clone()),
        "configuration_error" => AppError::Configuration(err.message.clone()),
        // An unrecognised code is by definition something this CLI
        // build doesn't know how to classify. Surface it as a generic
        // internal error rather than guessing a bucket — an unknown
        // code shouldn't quietly map to "not found".
        _ => return anyhow!("{}: {}", err.code, err.message),
    };
    anyhow::Error::from(app)
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,nagori=debug"));
    // `try_init` (instead of `init`) so callers in different branches
    // (daemon path vs. IPC-fallback warn) can both invoke this without
    // panicking on a double-set global subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

// The CLI dispatcher intentionally mirrors the subcommand enum one-to-one.
#[allow(clippy::too_many_lines)]
async fn run_local_command(cli: Cli) -> Result<()> {
    let format = OutputFormat::from(cli.json, cli.jsonl);

    // `Capabilities` is a static OS probe — short-circuit before we
    // touch the DB so users can inspect the host matrix on machines
    // where the SQLite path is misconfigured or unreadable.
    if matches!(cli.command, Command::Capabilities) {
        print_capabilities(&nagori_platform_native::capabilities(), format)?;
        return Ok(());
    }

    let db_path = cli.db.clone().unwrap_or_else(default_db_path);
    if let Some(parent) = db_path.parent() {
        nagori_storage::ensure_private_directory(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let store = SqliteStore::open(&db_path)
        .with_context(|| format!("failed to open {}", db_path.display()))?;

    match cli.command {
        Command::List(args) => {
            let entries = if args.pinned {
                store.list_pinned().await?
            } else {
                store.list_recent(args.limit).await?
            };
            print_entries(entries, format, args.include_sensitive)?;
        }
        Command::Search(args) => {
            let query = SearchQuery::new(&args.query, normalize_text(&args.query), args.limit);
            let results = store.search(query).await?;
            print_search_results(results, format)?;
        }
        Command::Get(args) => {
            let id = parse_id(&args.id)?;
            let entry = store
                .get(id)
                .await?
                .ok_or_else(|| anyhow::Error::new(AppError::NotFound))?;
            let include_text =
                args.include_sensitive || is_text_safe_for_default_output(entry.sensitivity);
            print_entry(&entry, format, include_text)?;
        }
        Command::Add(args) => {
            let text = read_text(args)?;
            let runtime = build_headless_runtime(store.clone())?;
            let id = runtime.add_text(text).await?;
            let entry = store
                .get(id)
                .await?
                .context("entry not found after insert")?;
            print_entry(
                &entry,
                format,
                is_text_safe_for_default_output(entry.sensitivity),
            )?;
        }
        Command::Delete(args) => {
            store.mark_deleted(parse_id(&args.id)?).await?;
            print_ack(format);
        }
        Command::Pin(args) => {
            store.set_pinned(parse_id(&args.id)?, true).await?;
            print_ack(format);
        }
        Command::Unpin(args) => {
            store.set_pinned(parse_id(&args.id)?, false).await?;
            print_ack(format);
        }
        Command::Copy(args) => {
            let id = parse_id(&args.id)?;
            let runtime = build_runtime(store.clone())?;
            runtime.copy_entry(id).await?;
            print_ack(format);
        }
        Command::Paste(args) => {
            let id = parse_id(&args.id)?;
            let runtime = build_runtime(store.clone())?;
            runtime.paste_entry(id, None).await?;
            print_ack(format);
        }
        Command::Clear(args) => {
            let cutoff = match clear_request_from_args(&args)? {
                ClearRequest::All => OffsetDateTime::now_utc(),
                ClearRequest::OlderThanDays { days } => {
                    OffsetDateTime::now_utc() - time::Duration::days(i64::from(days))
                }
            };
            let deleted = store.clear_older_than(cutoff).await?;
            print_clear_result(deleted, format);
        }
        Command::Quick(args) => {
            let runtime = build_headless_runtime(store.clone())?;
            let output = runtime
                .run_quick_action(parse_id(&args.id)?, args.action)
                .await?;
            print_ai_output(&output.into(), format)?;
        }
        Command::Ai(args) => {
            if matches!(args.action, AiActionId::Translate) && args.to.is_none() {
                anyhow::bail!("`nagori ai translate` requires --to <language> (e.g. --to ja)");
            }
            let options = AiRequestOptions {
                source_language: args.from.clone(),
                target_language: args.to.clone(),
                ..AiRequestOptions::default()
            };
            let runtime = build_headless_runtime(store.clone())?;
            run_ai_streaming(
                &runtime,
                parse_id(&args.id)?,
                args.action,
                options,
                !args.no_stream,
                format,
            )
            .await?;
        }
        Command::Doctor => {
            print_local_doctor(&db_path, &store).await?;
        }
        Command::Capabilities => unreachable!("handled before DB open"),
        Command::Daemon(args) => match args.command {
            DaemonCommand::Status => {
                let settings = store.get_settings().await?;
                print_status(&db_path, &settings, format)?;
            }
            DaemonCommand::Run(_) => unreachable!("handled before run_local_command"),
            DaemonCommand::Stop => {
                anyhow::bail!("daemon stop requires --ipc <socket>");
            }
        },
    }

    Ok(())
}

// On platforms without a native adapter the body short-circuits via
// `bail!`, so no `.await` runs.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows")),
    allow(clippy::unused_async)
)]
async fn run_daemon_command(cli: Cli) -> Result<()> {
    let Command::Daemon(DaemonArgs {
        command: DaemonCommand::Run(args),
    }) = cli.command
    else {
        unreachable!()
    };
    let db_path = cli.db.clone().unwrap_or_else(default_db_path);
    if let Some(parent) = db_path.parent() {
        nagori_storage::ensure_private_directory(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let store = SqliteStore::open(&db_path)
        .with_context(|| format!("failed to open {}", db_path.display()))?;

    let socket_path = cli.ipc.clone().unwrap_or_else(default_socket_path);
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
    // (and vice versa). The CLI's `run_ipc_command` mirrors this derivation
    // so client and daemon agree on the path.
    let token_path = nagori_ipc::token_path_for_endpoint(&socket_path);
    let defaults = DaemonConfig::default();
    let max_concurrent_connections = args
        .ipc_max_connections
        .unwrap_or(defaults.max_concurrent_connections);
    let config = DaemonConfig {
        socket_path,
        token_path,
        capture_interval: std::time::Duration::from_millis(args.capture_interval_ms),
        // The clap range above already keeps this well clear of overflow;
        // `saturating_mul` is belt-and-suspenders in case the bound is ever
        // relaxed without revisiting this arithmetic.
        maintenance_interval: std::time::Duration::from_secs(
            args.maintenance_interval_min.saturating_mul(60),
        ),
        secure_focus_fail_closed,
        max_concurrent_connections,
        ..defaults
    };

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        let parts = build_native_runtime(
            store,
            NativeRuntimeOptions {
                socket_path: Some(config.socket_path.clone()),
                ai_engine: None,
            },
        )?;
        run_daemon(
            parts.runtime,
            parts.clipboard_reader,
            config,
            Some(parts.window),
        )
        .await?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (store, config);
        anyhow::bail!("daemon run is only available on macOS, Windows, and Linux in this build")
    }
}

#[allow(clippy::too_many_lines)]
async fn run_ipc_command(cli: Cli, socket_path: PathBuf) -> Result<()> {
    // Derive the token path from the IPC endpoint so a CLI run with
    // `--ipc <custom>` reads the same file the matching daemon wrote.
    // Without this, a custom-endpoint daemon would still write the
    // default `nagori.token` and trample the token of any default-endpoint
    // daemon also running on this machine.
    let token_path = nagori_ipc::token_path_for_endpoint(&socket_path);
    let token = nagori_ipc::read_token_file(&token_path).map_err(|err| {
        anyhow!(
            "failed to read IPC auth token from {}: {err}. Is the daemon running?",
            token_path.display()
        )
    })?;
    let client = IpcClient::new(
        socket_path
            .to_str()
            .ok_or_else(|| anyhow!("ipc socket path must be valid UTF-8"))?,
        token,
    );
    let format = OutputFormat::from(cli.json, cli.jsonl);
    match cli.command {
        Command::List(args) => {
            let request = if args.pinned {
                IpcRequest::ListPinned(ListPinnedRequest {
                    include_sensitive: args.include_sensitive,
                })
            } else {
                IpcRequest::ListRecent(ListRecentRequest {
                    limit: args.limit,
                    include_sensitive: args.include_sensitive,
                })
            };
            let resp = client.send(request).await?;
            print_dto_entries(expect_entries(resp)?, format)?;
        }
        Command::Search(args) => {
            let resp = client
                .send(IpcRequest::Search(SearchRequest {
                    query: args.query,
                    limit: args.limit,
                }))
                .await?;
            print_dto_search(expect_search(resp)?, format)?;
        }
        Command::Get(args) => {
            let resp = client
                .send(IpcRequest::GetEntry(GetEntryRequest {
                    id: parse_id(&args.id)?,
                    include_sensitive: args.include_sensitive,
                }))
                .await?;
            print_dto_entry(&expect_entry(resp)?, format)?;
        }
        Command::Add(args) => {
            let text = read_text(args)?;
            let resp = client
                .send(IpcRequest::AddEntry(AddEntryRequest { text }))
                .await?;
            print_dto_entry(&expect_entry(resp)?, format)?;
        }
        Command::Delete(args) => {
            expect_ack(
                client
                    .send(IpcRequest::DeleteEntry(DeleteEntryRequest {
                        id: parse_id(&args.id)?,
                    }))
                    .await?,
            )?;
            print_ack(format);
        }
        Command::Pin(args) => {
            expect_ack(
                client
                    .send(IpcRequest::PinEntry(PinEntryRequest {
                        id: parse_id(&args.id)?,
                        pinned: true,
                    }))
                    .await?,
            )?;
            print_ack(format);
        }
        Command::Unpin(args) => {
            expect_ack(
                client
                    .send(IpcRequest::PinEntry(PinEntryRequest {
                        id: parse_id(&args.id)?,
                        pinned: false,
                    }))
                    .await?,
            )?;
            print_ack(format);
        }
        Command::Copy(args) => {
            expect_ack(
                client
                    .send(IpcRequest::CopyEntry(CopyEntryRequest {
                        id: parse_id(&args.id)?,
                    }))
                    .await?,
            )?;
            print_ack(format);
        }
        Command::Paste(args) => {
            expect_ack(
                client
                    .send(IpcRequest::PasteEntry(PasteEntryRequest {
                        id: parse_id(&args.id)?,
                        format: None,
                    }))
                    .await?,
            )?;
            print_ack(format);
        }
        Command::Quick(args) => {
            let resp = client
                .send(IpcRequest::RunQuickAction(RunQuickActionRequest {
                    id: parse_id(&args.id)?,
                    action: args.action,
                }))
                .await?;
            print_ai_output(&expect_ai_output(resp)?, format)?;
        }
        Command::Ai(args) => {
            // The daemon drives AI actions to completion over a one-shot
            // envelope — streaming runs in-process via the local path, so an
            // explicit `--ipc` connection always returns the final result.
            let resp = client
                .send(IpcRequest::RunAiAction(RunAiActionRequest {
                    id: parse_id(&args.id)?,
                    action: args.action,
                }))
                .await?;
            print_ai_output(&expect_ai_output(resp)?, format)?;
        }
        Command::Clear(args) => {
            let request = clear_request_from_args(&args)?;
            let resp = client.send(IpcRequest::Clear(request)).await?;
            print_clear_result(expect_cleared(resp)?.deleted, format);
        }
        Command::Doctor => {
            let resp = client.send(IpcRequest::Doctor).await?;
            print_doctor_report(&expect_doctor(resp)?, format)?;
        }
        Command::Capabilities => {
            let resp = client.send(IpcRequest::Capabilities).await?;
            print_capabilities(&expect_capabilities(resp)?, format)?;
        }
        Command::Daemon(args) => match args.command {
            DaemonCommand::Status => {
                let resp = client.send(IpcRequest::Health).await?;
                let IpcResponse::Health(health) = resp else {
                    anyhow::bail!("unexpected ipc response");
                };
                if format.is_json() {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "ok": health.ok,
                            "version": health.version,
                        }))?
                    );
                } else {
                    println!("ok\t{}", health.version);
                }
            }
            DaemonCommand::Stop => {
                expect_ack(client.send(IpcRequest::Shutdown).await?)?;
                print_ack(format);
            }
            DaemonCommand::Run(_) => unreachable!("handled before run_ipc_command"),
        },
    }
    Ok(())
}

fn build_runtime(store: SqliteStore) -> Result<NagoriRuntime> {
    Ok(build_native_runtime(store, NativeRuntimeOptions::default())?.runtime)
}

/// Build a runtime for CLI commands that don't touch the OS clipboard.
///
/// `add` and `ai` operate on the store and AI provider only; they never
/// invoke `ClipboardWriter::set_*` or `PasteController::paste_frontmost`.
/// Wire explicit `MemoryClipboard` / `NoopPasteController` so the builder
/// never sees missing adapters — the safety check in
/// `NagoriRuntimeBuilder::build` stays meaningful for paths that *do*
/// need real clipboard integration. The function still propagates a
/// `Result` so a future required adapter surfaces here as a user-facing
/// CLI error instead of a panic.
fn build_headless_runtime(store: SqliteStore) -> Result<NagoriRuntime> {
    let mut builder = NagoriRuntime::builder(store)
        .clipboard(Arc::new(MemoryClipboard::new()))
        .paste(Arc::new(NoopPasteController));
    // Wire the host's default AI engine (Apple Foundation Models on macOS) so
    // `nagori ai` can stream in-process without opening the OS clipboard.
    if let Some(engine) = nagori_platform_native::default_ai_engine() {
        builder = builder.ai_engine(engine);
    }
    Ok(builder.build()?)
}

/// Drives a model-backed AI action in-process and renders its event stream.
///
/// Cancellation is wired two ways: `Ctrl-C` cancels the in-flight request (and
/// the process exits 130 once the stream drains), and dropping the stream — for
/// example on a broken stdout pipe — cancels it through the runtime's request
/// registry guard.
async fn run_ai_streaming(
    runtime: &NagoriRuntime,
    id: EntryId,
    action: AiActionId,
    options: AiRequestOptions,
    stream: bool,
    format: OutputFormat,
) -> Result<()> {
    use std::io::Write;

    let run = runtime.start_ai_action(id, action, options).await?;
    let request_id = run.request_id;
    let mut events = run.events;

    let mut interrupted = false;
    let mut warnings: Vec<String> = Vec::new();
    let mut final_text = String::new();
    let mut buffer = String::new();
    let stdout = std::io::stdout();

    loop {
        let item = tokio::select! {
            biased;
            // Ctrl-C cancels the request through the registry; keep draining so
            // the stream reaches its terminal `Cancelled`.
            res = tokio::signal::ctrl_c(), if !interrupted => {
                res.ok();
                interrupted = true;
                let _ = runtime.cancel_ai_action(request_id);
                continue;
            }
            item = events.next() => item,
        };
        let Some(item) = item else { break };
        let event = item.map_err(|err| anyhow!("{:?}: {}", err.code, err.message))?;

        if stream && format.is_json() {
            let mut handle = stdout.lock();
            writeln!(handle, "{}", serde_json::to_string(&event)?)?;
        }
        match event {
            AiEvent::Delta { text, .. } => {
                buffer.push_str(&text);
                if stream && !format.is_json() {
                    let mut handle = stdout.lock();
                    write!(handle, "{text}")?;
                    handle.flush()?;
                }
            }
            AiEvent::Replace { text, .. } => {
                if stream && !format.is_json() {
                    let mut handle = stdout.lock();
                    writeln!(handle)?;
                    write!(handle, "{text}")?;
                    handle.flush()?;
                }
                buffer = text;
            }
            AiEvent::Done {
                final_text: text,
                warnings: done_warnings,
                ..
            } => {
                final_text = text;
                warnings = done_warnings;
                break;
            }
            AiEvent::Cancelled => break,
        }
    }

    if !stream || format.is_json() {
        // Non-streaming (or JSON Lines already emitted): print the authoritative
        // result once. The streaming text path already wrote to stdout.
        if !format.is_json() {
            let output = AiOutputDto {
                text: if final_text.is_empty() {
                    buffer.clone()
                } else {
                    final_text.clone()
                },
                created_entry: None,
                warnings: warnings.clone(),
            };
            print_ai_output(&output, format)?;
        }
    } else if !final_text.is_empty() || !buffer.is_empty() {
        // Terminate the streamed text line.
        let mut handle = stdout.lock();
        writeln!(handle)?;
    }

    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    if interrupted {
        // SIGINT: mirror shells' 128 + signal-number convention.
        std::process::exit(130);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum OutputFormat {
    Text,
    Json,
    Jsonl,
}

impl OutputFormat {
    const fn from(json: bool, jsonl: bool) -> Self {
        match (json, jsonl) {
            (_, true) => Self::Jsonl,
            (true, _) => Self::Json,
            _ => Self::Text,
        }
    }

    const fn is_json(self) -> bool {
        matches!(self, Self::Json | Self::Jsonl)
    }
}

fn read_text(args: AddArgs) -> Result<String> {
    if args.stdin {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        Ok(buffer)
    } else {
        args.text
            .ok_or_else(|| anyhow!("either --text or --stdin must be provided"))
    }
}

/// Environment variable that overrides the default DB path resolution.
///
/// Mirrors the desktop shell's startup-error recovery hint
/// (`apps/desktop/src-tauri/src/state.rs::annotate_startup_error`): if
/// the platform-default directory is unwritable the user can redirect
/// nagori to a path they control. Honoured here so the CLI and desktop
/// processes line up on the same store when both consult the variable.
const NAGORI_DB_PATH_ENV: &str = "NAGORI_DB_PATH";

fn default_db_path() -> PathBuf {
    resolve_default_db_path(std::env::var_os(NAGORI_DB_PATH_ENV), dirs::data_local_dir())
}

/// Pure path-resolution helper so unit tests don't have to mutate the
/// process environment (which is `unsafe` and races with parallel tests).
fn resolve_default_db_path(
    override_env: Option<std::ffi::OsString>,
    data_local_dir: Option<PathBuf>,
) -> PathBuf {
    if let Some(value) = override_env
        && !value.is_empty()
    {
        return PathBuf::from(value);
    }
    data_local_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
        .join("nagori.sqlite")
}

fn parse_id(value: &str) -> Result<EntryId> {
    EntryId::from_str(value)
        .map_err(|err| AppError::InvalidInput(format!("invalid entry id: {value}: {err}")).into())
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

fn expect_entry(response: IpcResponse) -> Result<EntryDto> {
    match response {
        IpcResponse::Entry(entry) => Ok(entry),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

fn expect_entries(response: IpcResponse) -> Result<Vec<EntryDto>> {
    match response {
        IpcResponse::Entries(entries) => Ok(entries),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

fn expect_search(response: IpcResponse) -> Result<SearchResponse> {
    match response {
        IpcResponse::Search(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

fn expect_ai_output(response: IpcResponse) -> Result<AiOutputDto> {
    match response {
        IpcResponse::AiOutput(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

fn expect_ack(response: IpcResponse) -> Result<()> {
    match response {
        IpcResponse::Ack => Ok(()),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

fn expect_cleared(response: IpcResponse) -> Result<ClearResponse> {
    match response {
        IpcResponse::Cleared(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

fn clear_request_from_args(args: &ClearArgs) -> Result<ClearRequest> {
    // The clap arg group enforces "exactly one of --all / --older-than-days",
    // so reaching this point with neither set means a clap bug or a manual
    // struct construction. Defend in depth.
    match (args.older_than_days, args.all) {
        (Some(days), false) => {
            let days = u32::try_from(days)
                .map_err(|_| AppError::InvalidInput("--older-than-days must be >= 0".into()))?;
            Ok(ClearRequest::OlderThanDays { days })
        }
        (None, true) => Ok(ClearRequest::All),
        _ => Err(AppError::InvalidInput("specify --all or --older-than-days".into()).into()),
    }
}

fn expect_doctor(response: IpcResponse) -> Result<DoctorReport> {
    match response {
        IpcResponse::Doctor(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

fn expect_capabilities(response: IpcResponse) -> Result<PlatformCapabilities> {
    match response {
        IpcResponse::Capabilities(value) => Ok(*value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_with_code(code: &str) -> nagori_ipc::IpcError {
        nagori_ipc::IpcError {
            code: code.to_owned(),
            message: format!("test message for {code}"),
            recoverable: false,
        }
    }

    #[test]
    fn exit_code_for_covers_every_apperror_variant() {
        // Pin the contract: each `AppError` variant must map to a
        // distinct exit code. Updating this table without updating the
        // CLI shell wrappers (and vice versa) is the kind of drift the
        // string-match fallback used to hide.
        let table = [
            (AppError::NotFound, 4),
            (AppError::InvalidInput("x".into()), 2),
            (AppError::Policy("x".into()), 5),
            (AppError::Permission("x".into()), 6),
            (AppError::Unsupported("x".into()), 7),
            (AppError::Storage("x".into()), 8),
            (AppError::Search("x".into()), 8),
            (AppError::Platform("x".into()), 8),
            (AppError::Ai("x".into()), 8),
            (AppError::Configuration("x".into()), 8),
        ];
        for (err, expected) in table {
            let label = format!("{err:?}");
            let wrapped = anyhow::Error::from(err);
            assert_eq!(exit_code_for(&wrapped), expected, "variant {label}");
        }
    }

    #[test]
    fn exit_code_for_unknown_error_is_internal_not_one() {
        // The previous implementation returned 1 for any anyhow error
        // that didn't downcast. That collided with shell convention
        // ("1 = generic failure") and made it impossible to tell a
        // logic bug apart from a routine "no match" exit. Internal
        // failures get 8, same bucket as unrecoverable AppError.
        let bare = anyhow!("some opaque error");
        assert_eq!(exit_code_for(&bare), 8);
    }

    #[test]
    fn ipc_error_to_anyhow_round_trips_each_known_code() {
        let cases = [
            ("not_found", 4_u8),
            ("invalid_input", 2),
            ("policy_error", 5),
            ("permission_error", 6),
            ("unsupported", 7),
            ("storage_error", 8),
            ("search_error", 8),
            ("platform_error", 8),
            ("ai_error", 8),
            ("configuration_error", 8),
        ];
        for (code, expected_exit) in cases {
            let err = err_with_code(code);
            let wrapped = ipc_error_to_anyhow(&err);
            assert_eq!(
                exit_code_for(&wrapped),
                expected_exit,
                "round-trip for code `{code}`",
            );
        }
    }

    #[test]
    fn ipc_error_to_anyhow_preserves_original_message() {
        // The structured `AppError` we synthesise must carry the
        // message the daemon sent so the user-facing display still
        // describes the actual failure (path, hint, etc.) and not just
        // the bucket.
        let err = err_with_code("policy_error");
        let wrapped = ipc_error_to_anyhow(&err);
        assert!(
            wrapped
                .to_string()
                .contains("test message for policy_error"),
            "rendered error must include daemon-supplied message, got: {wrapped}",
        );
    }

    #[test]
    fn ipc_error_to_anyhow_unknown_code_falls_through_to_internal() {
        // An unknown code from a future daemon must not silently map
        // to the wrong bucket — it ends up as a non-AppError anyhow,
        // which `exit_code_for` classifies as 8 (internal).
        let err = err_with_code("future_code_we_dont_know");
        let wrapped = ipc_error_to_anyhow(&err);
        assert!(wrapped.downcast_ref::<AppError>().is_none());
        assert_eq!(exit_code_for(&wrapped), 8);
    }

    /// The CLI tells users in `--help` and the desktop's startup error
    /// hint that `NAGORI_DB_PATH` redirects the store. Keep the resolver
    /// honest so that promise actually holds at runtime.
    #[test]
    fn resolve_default_db_path_honours_env_override() {
        let override_path = PathBuf::from("/custom/path/to/nagori.sqlite");
        let resolved = resolve_default_db_path(
            Some(override_path.as_os_str().to_owned()),
            Some(PathBuf::from("/should/be/ignored")),
        );
        assert_eq!(resolved, override_path);
    }

    #[test]
    fn resolve_default_db_path_treats_empty_env_as_unset() {
        let resolved = resolve_default_db_path(
            Some(std::ffi::OsString::new()),
            Some(PathBuf::from("/data/local")),
        );
        assert_eq!(resolved, PathBuf::from("/data/local/nagori/nagori.sqlite"));
    }

    #[test]
    fn resolve_default_db_path_uses_platform_default_when_env_unset() {
        let resolved = resolve_default_db_path(None, Some(PathBuf::from("/data/local")));
        assert_eq!(resolved, PathBuf::from("/data/local/nagori/nagori.sqlite"));
    }
}

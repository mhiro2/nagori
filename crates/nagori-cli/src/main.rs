use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use nagori_ai::LocalAiProvider;
use nagori_core::{
    AiActionId, AppError, AppSettings, ClipboardEntry, EntryId, EntryRepository, SearchQuery,
    SettingsRepository, is_text_safe_for_default_output, safe_preview_for_dto,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use nagori_daemon::run_daemon;
use nagori_daemon::{DaemonConfig, NagoriRuntime, default_socket_path};
use nagori_ipc::{
    AddEntryRequest, AiOutputDto, ClearRequest, ClearResponse, CopyEntryRequest,
    DeleteEntryRequest, DoctorReport, EntryDto, GetEntryRequest, IpcClient, IpcRequest,
    IpcResponse, ListPinnedRequest, ListRecentRequest, PasteEntryRequest, PinEntryRequest,
    RunAiActionRequest, SearchRequest, SearchResponse, SearchResultDto,
};
use nagori_search::normalize_text;
use nagori_storage::SqliteStore;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use nagori_platform::PermissionChecker;
#[cfg(target_os = "macos")]
use nagori_platform_macos::{
    MacosClipboard, MacosPasteController, MacosPermissionChecker, MacosWindowBehavior,
};
#[cfg(target_os = "windows")]
use nagori_platform_windows::{
    WindowsClipboard, WindowsPasteController, WindowsPermissionChecker, WindowsWindowBehavior,
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
    Ai(AiArgs),
    Doctor,
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
struct AiArgs {
    action: AiActionId,
    id: String,
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
    #[arg(long, default_value_t = 500)]
    capture_interval_ms: u64,
    #[arg(long, default_value_t = 30)]
    maintenance_interval_min: u64,
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
    matches!(
        cmd,
        Command::Add(_)
            | Command::Delete(_)
            | Command::Pin(_)
            | Command::Unpin(_)
            | Command::Copy(_)
            | Command::Paste(_)
            | Command::Clear(_)
            | Command::Ai(_)
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
            | AppError::Ai(_) => 8,
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
    let db_path = cli.db.clone().unwrap_or_else(default_db_path);
    if let Some(parent) = db_path.parent() {
        nagori_storage::ensure_private_directory(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let store = SqliteStore::open(&db_path)
        .with_context(|| format!("failed to open {}", db_path.display()))?;
    let format = OutputFormat::from(cli.json, cli.jsonl);

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
            let runtime = build_headless_runtime(store.clone());
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
        Command::Ai(args) => {
            let runtime = build_headless_runtime(store.clone());
            let output = runtime
                .run_ai_action(parse_id(&args.id)?, args.action)
                .await?;
            print_ai_output(&output.into(), format)?;
        }
        Command::Doctor => {
            print_local_doctor(&db_path, &store).await?;
        }
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
    let config = DaemonConfig {
        socket_path,
        token_path,
        capture_interval: std::time::Duration::from_millis(args.capture_interval_ms),
        maintenance_interval: std::time::Duration::from_secs(args.maintenance_interval_min * 60),
        secure_focus_fail_closed,
        ..DaemonConfig::default()
    };

    #[cfg(target_os = "macos")]
    {
        let clipboard = Arc::new(MacosClipboard::new()?);
        let window: Arc<dyn nagori_platform::WindowBehavior> = Arc::new(MacosWindowBehavior::new());
        let permissions: Arc<dyn PermissionChecker> = Arc::new(MacosPermissionChecker);
        let runtime = NagoriRuntime::builder(store)
            .clipboard(clipboard.clone())
            .paste(Arc::new(MacosPasteController))
            .ai(Arc::new(LocalAiProvider::default()))
            .permissions(permissions)
            .socket_path(config.socket_path.clone())
            .build();
        run_daemon(runtime, clipboard, config, Some(window)).await?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let clipboard = Arc::new(WindowsClipboard::new()?);
        let window: Arc<dyn nagori_platform::WindowBehavior> =
            Arc::new(WindowsWindowBehavior::new());
        let permissions: Arc<dyn PermissionChecker> = Arc::new(WindowsPermissionChecker);
        let runtime = NagoriRuntime::builder(store)
            .clipboard(clipboard.clone())
            .paste(Arc::new(WindowsPasteController))
            .ai(Arc::new(LocalAiProvider::default()))
            .permissions(permissions)
            .socket_path(config.socket_path.clone())
            .build();
        run_daemon(runtime, clipboard, config, Some(window)).await?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (store, config);
        anyhow::bail!("daemon run is only available on macOS and Windows in this build")
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
        Command::Ai(args) => {
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

// On platforms without a native adapter the body never returns Err, but
// the macOS / Windows paths need `?`.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "windows")),
    allow(clippy::unnecessary_wraps)
)]
fn build_runtime(store: SqliteStore) -> Result<NagoriRuntime> {
    #[cfg(target_os = "macos")]
    {
        let clipboard = Arc::new(MacosClipboard::new()?);
        let permissions: Arc<dyn PermissionChecker> = Arc::new(MacosPermissionChecker);
        Ok(NagoriRuntime::builder(store)
            .clipboard(clipboard)
            .paste(Arc::new(MacosPasteController))
            .ai(Arc::new(LocalAiProvider::default()))
            .permissions(permissions)
            .build())
    }
    #[cfg(target_os = "windows")]
    {
        let clipboard = Arc::new(WindowsClipboard::new()?);
        let permissions: Arc<dyn PermissionChecker> = Arc::new(WindowsPermissionChecker);
        Ok(NagoriRuntime::builder(store)
            .clipboard(clipboard)
            .paste(Arc::new(WindowsPasteController))
            .ai(Arc::new(LocalAiProvider::default()))
            .permissions(permissions)
            .build())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(NagoriRuntime::builder(store)
            .ai(Arc::new(LocalAiProvider::default()))
            .build())
    }
}

fn build_headless_runtime(store: SqliteStore) -> NagoriRuntime {
    NagoriRuntime::builder(store)
        .ai(Arc::new(LocalAiProvider::default()))
        .build()
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

fn default_db_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
        .join("nagori.sqlite")
}

fn parse_id(value: &str) -> Result<EntryId> {
    EntryId::from_str(value)
        .map_err(|err| AppError::InvalidInput(format!("invalid entry id: {value}: {err}")).into())
}

fn print_entries(
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

fn print_entry(entry: &ClipboardEntry, format: OutputFormat, include_text: bool) -> Result<()> {
    match format {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&entry_json(entry, include_text)?)?
        ),
        OutputFormat::Jsonl => println!(
            "{}",
            serde_json::to_string(&entry_json(entry, include_text)?)?
        ),
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

fn print_search_results(
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

fn print_dto_entries(entries: Vec<EntryDto>, format: OutputFormat) -> Result<()> {
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

fn print_dto_entry(entry: &EntryDto, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(entry)?),
        OutputFormat::Jsonl => println!("{}", serde_json::to_string(entry)?),
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

fn print_dto_search(response: SearchResponse, format: OutputFormat) -> Result<()> {
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

fn print_clear_result(deleted: usize, format: OutputFormat) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::json!({ "deleted": deleted }));
        }
        OutputFormat::Text => println!("deleted {deleted}"),
    }
}

/// Replace the user's home prefix on `path` with `~` when rendering for
/// human consumption. `nagori doctor` prints DB / socket / token paths
/// to stdout, which routinely shows up in shared terminals, paired
/// programming sessions, and screenshots posted to issue trackers — and
/// the absolute path is just the username with extra steps. The JSON /
/// JSONL paths still emit the full value untouched so automation can
/// parse them without re-expanding `~`.
fn shorten_home(path: &Path) -> String {
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

fn print_doctor_report(report: &DoctorReport, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string_pretty(report)?);
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
            println!("local_only_mode\t{}", report.local_only_mode);
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
        }
    }
    Ok(())
}

async fn print_local_doctor(db_path: &Path, store: &SqliteStore) -> Result<()> {
    let settings = store.get_settings().await?;
    let provider_label = match &settings.ai_provider {
        nagori_core::settings::AiProviderSetting::None => "none".to_owned(),
        nagori_core::settings::AiProviderSetting::Local => "local".to_owned(),
        nagori_core::settings::AiProviderSetting::Remote { name } => format!("remote:{name}"),
    };
    println!("version\t{}", env!("CARGO_PKG_VERSION"));
    println!("version_latest\t(unknown)");
    println!("update_channel\t{}", settings.update_channel.as_str());
    println!("db\t{}", shorten_home(db_path));
    println!("capture_enabled\t{}", settings.capture_enabled);
    println!("auto_paste_enabled\t{}", settings.auto_paste_enabled);
    println!("ai_enabled\t{}", settings.ai_enabled);
    println!("local_only_mode\t{}", settings.local_only_mode);
    println!("ai_provider\t{provider_label}");
    #[cfg(target_os = "macos")]
    {
        let checker = MacosPermissionChecker;
        if let Ok(statuses) = checker.check().await {
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
        if let Ok(statuses) = checker.check().await {
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
    Ok(())
}

fn print_dto_search_row(result: &SearchResultDto) {
    println!(
        "{}\t{:.1}\t{:?}\t{}",
        result.id, result.score, result.kind, result.preview
    );
}

fn entry_json(entry: &ClipboardEntry, include_text: bool) -> Result<serde_json::Value> {
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

fn format_json_time(value: OffsetDateTime) -> Result<String> {
    value.format(&Rfc3339).map_err(Into::into)
}

fn print_ack(format: OutputFormat) {
    if format.is_json() {
        println!("{}", serde_json::json!({ "ok": true }));
    } else {
        println!("ok");
    }
}

fn print_ai_output(output: &AiOutputDto, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!("{}", serde_json::to_string_pretty(output)?);
        }
        OutputFormat::Text => {
            println!("{}", output.text);
            for warning in &output.warnings {
                eprintln!("warning: {warning}");
            }
        }
    }
    Ok(())
}

fn print_status(db_path: &Path, settings: &AppSettings, format: OutputFormat) -> Result<()> {
    if format.is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": true,
                "db": db_path,
                "capture_enabled": settings.capture_enabled,
                "ai_enabled": settings.ai_enabled,
                "auto_paste_enabled": settings.auto_paste_enabled,
                "history_retention_count": settings.history_retention_count,
            }))?
        );
    } else {
        println!("ok\t{}", db_path.display());
    }
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
}

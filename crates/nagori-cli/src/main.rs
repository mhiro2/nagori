use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use nagori_core::{AiActionId, AppError, QuickActionId};
use nagori_daemon::default_socket_path;
use nagori_ipc::{IpcClient, IpcRequest};

mod commands;
mod output;

use commands::{Executor, IpcContext, LocalContext};

#[derive(Debug, Clone, Parser)]
#[command(name = "nagori")]
#[command(about = "Local-first clipboard history CLI")]
struct Cli {
    /// Operate directly on this DB file (repair / offline mode). Reads are
    /// always allowed; write commands take the single-instance lock first
    /// and are refused while a desktop app or daemon owns the DB.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Path to the IPC endpoint of a running desktop app or daemon
    /// (Unix socket / Windows named pipe). Forces IPC: the command fails
    /// when the endpoint is unreachable.
    #[arg(long, global = true)]
    ipc: Option<PathBuf>,
    /// For read commands: try the default IPC endpoint first and fall back
    /// to reading the local DB when unreachable. Write commands route
    /// automatically (direct write when no instance is running, IPC
    /// otherwise), so the flag only changes reads.
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

#[derive(Debug, Clone, Subcommand)]
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

#[derive(Debug, Clone, Args)]
struct ListArgs {
    /// Maximum number of recent entries to show. Must be at least 1 — `0`
    /// would print nothing. Has no effect together with `--pinned`, which
    /// always returns the full pinned set.
    #[arg(long, default_value_t = 20, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
    limit: usize,
    /// List the full set of pinned entries instead of recent ones. `--limit`
    /// does not apply to this listing.
    #[arg(long)]
    pinned: bool,
    /// Include full text for Private/Secret entries.
    #[arg(long)]
    include_sensitive: bool,
}

#[derive(Debug, Clone, Args)]
struct SearchArgs {
    query: String,
    /// Maximum number of results to return. Must be at least 1 — `0` would
    /// print nothing.
    #[arg(long, default_value_t = 50, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
    limit: usize,
}

#[derive(Debug, Clone, Args)]
struct GetArgs {
    id: String,
    #[arg(long)]
    include_sensitive: bool,
}

#[derive(Debug, Clone, Args)]
struct AddArgs {
    #[arg(long, conflicts_with = "stdin")]
    text: Option<String>,
    #[arg(long)]
    stdin: bool,
}

#[derive(Debug, Clone, Args)]
struct IdArgs {
    id: String,
}

#[derive(Debug, Clone, Args)]
#[command(group = clap::ArgGroup::new("clear_scope").required(true).args(&["older_than_days", "all"]))]
struct ClearArgs {
    #[arg(long)]
    older_than_days: Option<i64>,
    /// Wipe every unpinned entry. Required when no time window is given.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Clone, Args)]
struct QuickArgs {
    action: QuickActionId,
    id: String,
}

#[derive(Debug, Clone, Args)]
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

#[derive(Debug, Clone, Args)]
struct DaemonArgs {
    #[command(subcommand)]
    command: DaemonCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum DaemonCommand {
    Status,
    Run(DaemonRunArgs),
    Stop,
}

#[derive(Debug, Clone, Args)]
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

/// Decide *where* the command runs — the daemon bootstrap, a forced IPC
/// endpoint, the instance-lock-gated write routing, or the local store —
/// then hand off to [`commands::run_cli`]. This function owns routing and
/// lock acquisition only; the per-command behaviour lives in `commands/`.
async fn dispatch(cli: Cli) -> Result<()> {
    if let Command::Daemon(DaemonArgs {
        command: DaemonCommand::Run(args),
    }) = cli.command
    {
        init_tracing();
        return commands::daemon::run_server(cli.db, cli.ipc, args).await;
    }
    if let Some(socket) = cli.ipc.clone() {
        return run_over_ipc(cli, &socket).await;
    }
    let writes = is_write_command(&cli.command);
    if writes && cli.db.is_none() {
        // Route the write by the single-instance lock, decided once:
        // acquiring it proves nothing owns the store (a direct write
        // cannot desync anyone), failing it proves an owner exists (so
        // the write must go through its IPC endpoint). Probing the
        // endpoint first would leave a gap between the health check and
        // the command's own connection in which the owner can exit.
        if let Some(lock) = try_acquire_direct_write_lock(&default_db_path())? {
            init_tracing();
            tracing::warn!(
                "ipc_fallback_to_local_db reason=no_running_instance mode=write-fallback"
            );
            let _write_lock = lock;
            return run_locally(cli).await;
        }
        let candidate = default_socket_path();
        return run_over_ipc(cli, &candidate).await.with_context(|| {
            "a running nagori owns the store but its IPC endpoint was unreachable. \
             Enable Settings → CLI (cli_ipc_enabled) in the desktop app or start \
             `nagori daemon run`, or quit the running instance to write to the DB \
             directly."
                .to_owned()
        });
    }
    // `--auto-ipc` only changes *reads* (see the flag's help): non-read
    // commands ignore it and route exactly as they would without it. Gating
    // the whole IPC attempt on the read predicate keeps that contract — and
    // guarantees the fallback below only ever re-runs an idempotent read,
    // never an action like `ai` (which may create an entry) or a control
    // command like `daemon stop` (whose success closes the connection).
    if cli.db.is_none() && cli.auto_ipc && can_fall_back_to_local_read(&cli.command) {
        let candidate = default_socket_path();
        if let Ok(token_path) = nagori_ipc::token_path_for_endpoint(&candidate)
            && let Ok(token) = nagori_ipc::read_token_file(&token_path)
            && IpcClient::new(candidate.to_string_lossy().as_ref(), token)
                .send(IpcRequest::Health)
                .await
                .is_ok()
        {
            // The probe succeeded, but the owner can still exit in the window
            // between it and the command's own connection (a probe→connect
            // TOCTOU). If that race makes the endpoint unreachable mid-flight,
            // fall through to the local read below instead of failing the
            // command. A *logical* error from a reachable daemon (NotFound, …)
            // is surfaced as-is, never masked by a silent local re-run.
            match run_over_ipc(cli.clone(), &candidate).await {
                Ok(()) => return Ok(()),
                Err(err) if is_ipc_transport_error(&err) => {
                    init_tracing();
                    tracing::warn!(
                        socket = %candidate.display(),
                        "ipc_fallback_to_local_db reason=owner_exited_after_probe mode=local-fallback"
                    );
                }
                Err(err) => return Err(err),
            }
        } else {
            // The endpoint either had no readable token or failed the health
            // probe. Reads fall back to opening the SQLite file directly —
            // safe against a concurrent owner, but any cache invalidation
            // we'd normally trigger via IPC isn't delivered. Surface the
            // fallback at warn! (stderr) so a user debugging stale results
            // sees the mismatch instead of having to bisect why their query
            // lags.
            init_tracing();
            tracing::warn!(
                socket = %candidate.display(),
                "ipc_fallback_to_local_db reason=endpoint_unreachable mode=local-fallback"
            );
        }
    }
    // Explicit `--db` writes are gated on the same lock; a held lock means
    // a running instance owns that store and the write must not bypass it.
    let _write_lock = if writes {
        let db_path = cli.db.clone().unwrap_or_else(default_db_path);
        match try_acquire_direct_write_lock(&db_path)? {
            Some(lock) => Some(lock),
            None => anyhow::bail!(
                "a running nagori (desktop app or daemon) owns {}. Write through it \
                 instead (drop --db), or quit it before writing to the DB directly.",
                db_path.display()
            ),
        }
    } else {
        None
    };
    run_locally(cli).await
}

async fn run_over_ipc(cli: Cli, socket_path: &Path) -> Result<()> {
    let executor = Executor::Ipc(IpcContext::connect(socket_path)?);
    commands::run_cli(cli, &executor).await
}

async fn run_locally(cli: Cli) -> Result<()> {
    let db_path = cli.db.clone().unwrap_or_else(default_db_path);
    let executor = Executor::Local(LocalContext { db_path });
    commands::run_cli(cli, &executor).await
}

/// Try to take the single-instance lock over the DB's directory before a
/// direct write — the same `nagori.lock` the desktop shell and the daemon
/// hold for their lifetime, so all three surfaces contend for one gate. A
/// held lock means writing here would land in `SQLite` without ever
/// invalidating the owner's search cache or refreshing its palette.
///
/// The lock directory is derived from the *canonicalized* DB path: locking
/// the lexical parent would let an alias (`--db` through a file or
/// directory symlink) acquire a different `nagori.lock` than the owner of
/// the real directory and bypass the gate. A DB that doesn't exist yet
/// (fresh store) can't be canonicalized itself, so its parent is resolved
/// instead and the filename re-attached.
fn try_acquire_direct_write_lock(db_path: &Path) -> Result<Option<nagori_storage::ProcessLock>> {
    let lexical_parent = match db_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    nagori_storage::ensure_private_directory(lexical_parent)
        .with_context(|| format!("failed to create {}", lexical_parent.display()))?;
    let resolved_db = std::fs::canonicalize(db_path).unwrap_or_else(|_| {
        let resolved_parent =
            std::fs::canonicalize(lexical_parent).unwrap_or_else(|_| lexical_parent.to_path_buf());
        match db_path.file_name() {
            Some(name) => resolved_parent.join(name),
            None => resolved_parent,
        }
    });
    let lock_dir = resolved_db
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(nagori_storage::ProcessLock::try_acquire(lock_dir)?)
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

/// Whether a command is a pure, side-effect-free read that `--auto-ipc` may
/// route to the daemon and, on an unreachable endpoint, re-run against the
/// local store.
///
/// This gates the whole `--auto-ipc` IPC attempt, honouring the flag's
/// contract that it "only changes reads": non-read commands ignore it and run
/// exactly as they would without it. It also keeps the local fallback safe —
/// `ai` may create an entry (its `Done` carries a `created_entry`) and is
/// expensive, so a re-run could double the work while the daemon is still
/// generating; `quick` is an action rather than a read; `daemon stop` is a
/// control command whose very success closes the connection. The write
/// commands never reach this path (they route by the instance lock).
const fn can_fall_back_to_local_read(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::List(_)
            | Command::Search(_)
            | Command::Get(_)
            | Command::Doctor
            | Command::Capabilities
            | Command::Daemon(DaemonArgs {
                command: DaemonCommand::Status,
            })
    )
}

/// Whether a `run_over_ipc` failure means the endpoint became unreachable —
/// the owner exited in the probe→connect window, or the socket/token vanished
/// — as opposed to a logical error the daemon returned (`NotFound`, `Policy`, …).
///
/// Transport failures surface as [`AppError::Platform`]: the client's connect
/// / connect-timeout / request-timeout paths raise it, and
/// [`IpcContext::connect`](commands::IpcContext) maps a vanished token file to
/// it too. Combined with [`can_fall_back_to_local_read`], the only commands
/// this gates are pure reads — which never produce a daemon-side
/// `platform_error` — so an `AppError::Platform` here always means the
/// exchange failed at the transport, not a logical error worth surfacing.
/// Re-running such a read locally is idempotent, so even a post-connect
/// transport failure (a slow/wedged daemon hitting the request timeout) safely
/// degrades to the authoritative on-disk read the `--auto-ipc` contract
/// promises.
fn is_ipc_transport_error(err: &anyhow::Error) -> bool {
    matches!(err.downcast_ref::<AppError>(), Some(AppError::Platform(_)))
}

fn exit_code_for(err: &anyhow::Error) -> u8 {
    if let Some(app) = err.downcast_ref::<AppError>() {
        return match app {
            AppError::NotFound => 4,
            AppError::Policy(_) => 5,
            AppError::InvalidInput(_) => 2,
            AppError::Permission(_) => 6,
            AppError::Unsupported(_) => 7,
            AppError::Storage { .. }
            | AppError::Search { .. }
            | AppError::Platform(_)
            | AppError::Ai(_)
            | AppError::Paste { .. }
            | AppError::Conflict(_)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_limited_to_pure_reads() {
        // Under `--auto-ipc`, only pure reads may re-run against the local
        // store on an IPC transport failure. Actions (`ai` can create an
        // entry; `quick` is a transform) and the `daemon stop` control command
        // must surface the IPC error instead of being silently re-run — else a
        // successful `daemon stop` whose response drops would report exit 2,
        // and an `ai` timeout would double-execute the generation.
        let cmd = |args: &[&str]| Cli::try_parse_from(args).expect("parse").command;
        for read in [
            vec!["nagori", "list"],
            vec!["nagori", "search", "q"],
            vec!["nagori", "get", "id"],
            vec!["nagori", "doctor"],
            vec!["nagori", "capabilities"],
            vec!["nagori", "daemon", "status"],
        ] {
            assert!(
                can_fall_back_to_local_read(&cmd(&read)),
                "{read:?} should be local-fallback eligible"
            );
        }
        for non_read in [
            vec!["nagori", "ai", "summarize", "id"],
            vec!["nagori", "quick", "format-json", "id"],
            vec!["nagori", "daemon", "stop"],
        ] {
            assert!(
                !can_fall_back_to_local_read(&cmd(&non_read)),
                "{non_read:?} must surface the IPC error, not fall back"
            );
        }
    }
    use anyhow::anyhow;
    use commands::ipc_error_to_anyhow;

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
            (AppError::storage("x"), 8),
            (AppError::search("x"), 8),
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

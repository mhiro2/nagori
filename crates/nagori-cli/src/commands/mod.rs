//! Per-command implementations, one module per subcommand.
//!
//! Every command runs against an [`Executor`]: either the local store
//! directly or a running instance's IPC endpoint. The routing decision —
//! locks, fallbacks, endpoint resolution — is made once in `main.rs`'s
//! `dispatch`; each command module then carries its local and IPC arms side
//! by side, so adding or changing a command touches a single file instead
//! of two parallel `match` blocks.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use nagori_core::{AppError, EntryId};
use nagori_daemon::NagoriRuntime;
use nagori_ipc::{
    AiOutputDto, ClearResponse, DoctorReport, EntryDto, IpcClient, IpcResponse, SearchResponse,
};
use nagori_platform::{MemoryClipboard, NoopPasteController, PlatformCapabilities};
use nagori_platform_native::{NativeRuntimeOptions, build_native_runtime};
use nagori_storage::SqliteStore;

use crate::{Cli, Command, DaemonArgs, DaemonCommand, OutputFormat};

pub mod add;
pub mod ai;
pub mod capabilities;
pub mod clear;
pub mod copy;
pub mod daemon;
pub mod delete;
pub mod doctor;
pub mod get;
pub mod list;
pub mod paste;
pub mod pin;
pub mod quick;
pub mod search;

/// How a command executes: against the local `SQLite` store directly, or
/// through a running desktop app / daemon's IPC endpoint.
pub enum Executor {
    Local(LocalContext),
    Ipc(IpcContext),
}

/// Direct-store execution context. The store is opened lazily so commands
/// that never touch the DB (`capabilities`) work even when the `SQLite`
/// path is misconfigured or unreadable.
pub struct LocalContext {
    pub db_path: PathBuf,
}

impl LocalContext {
    pub fn open_store(&self) -> Result<SqliteStore> {
        if let Some(parent) = self.db_path.parent() {
            nagori_storage::ensure_private_directory(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        SqliteStore::open(&self.db_path)
            .with_context(|| format!("failed to open {}", self.db_path.display()))
    }
}

/// IPC execution context: an authenticated client for the endpoint.
pub struct IpcContext {
    pub client: IpcClient,
}

impl IpcContext {
    /// Connect to `socket_path`, reading the auth token from the sibling
    /// file. The token path is derived from the IPC endpoint so a CLI run
    /// with `--ipc <custom>` reads the same file the matching daemon wrote
    /// — without this, a custom-endpoint daemon would still write the
    /// default `nagori.token` and trample the token of any default-endpoint
    /// daemon also running on this machine.
    pub fn connect(socket_path: &Path) -> Result<Self> {
        let token_path = nagori_ipc::token_path_for_endpoint(socket_path)
            .map_err(|err| anyhow!("failed to resolve the IPC auth token path: {err}"))?;
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
        Ok(Self { client })
    }
}

/// Route a parsed command onto its implementation module.
pub async fn run(command: Command, executor: &Executor, format: OutputFormat) -> Result<()> {
    match command {
        Command::List(args) => list::run(executor, &args, format).await,
        Command::Search(args) => search::run(executor, args, format).await,
        Command::Get(args) => get::run(executor, &args, format).await,
        Command::Add(args) => add::run(executor, args, format).await,
        Command::Delete(args) => delete::run(executor, &args, format).await,
        Command::Pin(args) => pin::run(executor, &args, true, format).await,
        Command::Unpin(args) => pin::run(executor, &args, false, format).await,
        Command::Copy(args) => copy::run(executor, &args, format).await,
        Command::Paste(args) => paste::run(executor, &args, format).await,
        Command::Clear(args) => clear::run(executor, &args, format).await,
        Command::Quick(args) => quick::run(executor, &args, format).await,
        Command::Ai(args) => ai::run(executor, &args, format).await,
        Command::Doctor => doctor::run(executor, format).await,
        Command::Capabilities => capabilities::run(executor, format).await,
        Command::Daemon(DaemonArgs { command }) => match command {
            DaemonCommand::Status => daemon::status(executor, format).await,
            DaemonCommand::Stop => daemon::stop(executor, format).await,
            DaemonCommand::Run(_) => unreachable!("handled before command dispatch"),
        },
    }
}

/// Compute the executor's output format and run the command. Thin entry
/// point for `main.rs` so the routing function above stays the single place
/// that maps `Command` variants onto modules.
pub async fn run_cli(cli: Cli, executor: &Executor) -> Result<()> {
    let format = OutputFormat::from(cli.json, cli.jsonl);
    run(cli.command, executor, format).await
}

pub(crate) fn parse_id(value: &str) -> Result<EntryId> {
    EntryId::from_str(value)
        .map_err(|err| AppError::InvalidInput(format!("invalid entry id: {value}: {err}")).into())
}

pub(crate) fn build_runtime(store: SqliteStore) -> Result<NagoriRuntime> {
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
pub(crate) fn build_headless_runtime(store: SqliteStore) -> Result<NagoriRuntime> {
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

/// Translate an IPC-level error response into an `anyhow::Error` whose
/// root cause is the structured `AppError`. Without this, the CLI's
/// `exit_code_for` would only see the rendered `"<code>: <message>"`
/// string and fall through to the internal-error bucket.
pub(crate) fn ipc_error_to_anyhow(err: &nagori_ipc::IpcError) -> anyhow::Error {
    let app = match err.code.as_str() {
        "not_found" => AppError::NotFound,
        "invalid_input" => AppError::InvalidInput(err.message.clone()),
        "policy_error" => AppError::Policy(err.message.clone()),
        "permission_error" => AppError::Permission(err.message.clone()),
        "unsupported" => AppError::Unsupported(err.message.clone()),
        "storage_error" => AppError::storage(err.message.clone()),
        "search_error" => AppError::search(err.message.clone()),
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

pub(crate) fn expect_entry(response: IpcResponse) -> Result<EntryDto> {
    match response {
        IpcResponse::Entry(entry) => Ok(entry),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

pub(crate) fn expect_entries(response: IpcResponse) -> Result<Vec<EntryDto>> {
    match response {
        IpcResponse::Entries(entries) => Ok(entries),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

pub(crate) fn expect_search(response: IpcResponse) -> Result<SearchResponse> {
    match response {
        IpcResponse::Search(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

pub(crate) fn expect_ai_output(response: IpcResponse) -> Result<AiOutputDto> {
    match response {
        IpcResponse::AiOutput(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

pub(crate) fn expect_ack(response: IpcResponse) -> Result<()> {
    match response {
        IpcResponse::Ack => Ok(()),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

pub(crate) fn expect_cleared(response: IpcResponse) -> Result<ClearResponse> {
    match response {
        IpcResponse::Cleared(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

pub(crate) fn expect_doctor(response: IpcResponse) -> Result<DoctorReport> {
    match response {
        IpcResponse::Doctor(value) => Ok(value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

pub(crate) fn expect_capabilities(response: IpcResponse) -> Result<PlatformCapabilities> {
    match response {
        IpcResponse::Capabilities(value) => Ok(*value),
        IpcResponse::Error(err) => Err(ipc_error_to_anyhow(&err)),
        _ => Err(anyhow!("unexpected ipc response")),
    }
}

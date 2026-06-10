//! IPC server transport and health observability.
//!
//! Split by concern so each transport and the shared observability surface
//! can evolve without dragging the others along:
//!
//! - [`health`] — [`IpcServerConfig`], [`IpcServerHealth`], and the
//!   panic-message redactor behind the doctor / health surfaces.
//! - [`connection`] — the transport-agnostic per-connection driver (bounded
//!   read, auth check, bounded write-back) shared by every platform.
//! - [`accept`] — the accept-loop scaffolding (permit-vs-shutdown race,
//!   two-stage handler drain) shared by both transports.
//! - `unix` — the Unix-domain-socket listener, accept loops, and bind helper.
//! - `windows` — the Windows named-pipe equivalents.
//!
//! The public surface is re-exported here unchanged so `lib.rs` and external
//! callers keep importing `nagori_ipc::{serve_unix, accept_loop_with_shutdown, …}`.

mod health;
pub use health::{IpcServerConfig, IpcServerHealth};

#[cfg(any(unix, windows))]
mod accept;
#[cfg(any(unix, windows))]
mod connection;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{
    accept_loop, accept_loop_with_shutdown, bind_unix, bind_unix_replacing_stale, serve_unix,
    serve_unix_with_health,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{DEFAULT_PIPE_NAME, accept_loop_pipe_with_shutdown, bind_pipe, serve_pipe};
// `serve_unix` must resolve on every platform because `lib.rs` re-exports it
// unconditionally. On Windows the stub lives beside the named-pipe transport.
#[cfg(all(windows, not(unix)))]
pub use windows::serve_unix;

/// Fallback `serve_unix` for platforms with neither a Unix-domain socket nor
/// a named pipe. Kept here rather than in a transport module because no
/// transport module compiles on these targets.
#[cfg(not(any(unix, windows)))]
pub async fn serve_unix<F, Fut>(
    _path: impl AsRef<std::path::Path>,
    _token: crate::AuthToken,
    _handler: F,
) -> nagori_core::Result<()>
where
    F: Fn(crate::IpcRequest) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = crate::IpcResponse> + Send + 'static,
{
    Err(nagori_core::AppError::Unsupported(
        "IPC server is not available on this platform".to_owned(),
    ))
}

pub mod client;
pub mod protocol;
pub mod server;
pub mod token;

/// Shared newline-delimited frame reader used by both the client and the
/// server transport so the wire framing (and its size boundary) can't drift
/// between the two.
mod framing;
#[cfg(windows)]
pub(crate) mod windows_security;

pub use client::IpcClient;
pub use protocol::*;
#[cfg(windows)]
pub use server::{DEFAULT_PIPE_NAME, accept_loop_pipe_with_shutdown, bind_pipe, serve_pipe};
pub use server::{IpcServerConfig, IpcServerHealth, serve_unix};
#[cfg(unix)]
pub use server::{accept_loop, accept_loop_with_shutdown, bind_unix, bind_unix_replacing_stale};
pub use token::{
    AuthToken, default_token_path, read_token_file, token_path_for_endpoint, write_token_file,
};

pub use nagori_core::MAX_IPC_BYTES;

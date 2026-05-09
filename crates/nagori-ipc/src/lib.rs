pub mod client;
pub mod protocol;
pub mod server;
pub mod token;

pub use client::IpcClient;
pub use protocol::*;
pub use server::serve_unix;
#[cfg(unix)]
pub use server::{accept_loop, accept_loop_with_shutdown, bind_unix};
pub use token::{AuthToken, default_token_path, read_token_file, write_token_file};

pub use nagori_core::MAX_IPC_BYTES;

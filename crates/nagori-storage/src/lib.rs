// rusqlite binds integers as `i64`; storage code casts `usize` deliberately.
#![allow(clippy::cast_possible_wrap)]
// Storage methods stay async to allow future `spawn_blocking` without API churn.
#![allow(clippy::unused_async)]

mod process_lock;
mod sqlite;

pub use process_lock::{ProcessLock, resolve_db_path, store_lock_dir};
#[cfg(feature = "semantic-index")]
pub use sqlite::{PendingEmbedding, SemanticIndexCounts};
pub use sqlite::{SqliteStore, ensure_private_directory};

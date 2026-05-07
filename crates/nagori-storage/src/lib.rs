// rusqlite binds integers as `i64`; storage code casts `usize` deliberately.
#![allow(clippy::cast_possible_wrap)]
// Storage methods stay async to allow future `spawn_blocking` without API churn.
#![allow(clippy::unused_async)]

mod sqlite;

pub use sqlite::{SqliteStore, ensure_private_directory};

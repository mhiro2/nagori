//! Unit tests for [`SqliteStore`], grouped by category. They live as
//! `#[cfg(test)]` submodules (not `tests/` integration tests) because they
//! exercise crate-private APIs such as the connection pool and migrations.

mod audit;
mod entries;
mod permissions;
mod representations;
mod schema;
mod search;
mod settings;
mod thumbnails;

use nagori_core::{EntryFactory, EntryId, EntryRepository};
use nagori_search::normalize_text;
use rusqlite::params;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::*;

async fn insert_text(store: &SqliteStore, text: &str) -> EntryId {
    let mut entry = EntryFactory::from_text(text);
    entry.search.normalized_text = normalize_text(entry.plain_text().unwrap());
    store.insert(entry).await.unwrap()
}

/// Backdate the `created_at` timestamp on a row so that retention
/// windows (`clear_older_than`) and `enforce_retention_count` ordering
/// can be tested deterministically without sleeping.
fn backdate_entry(store: &SqliteStore, id: EntryId, when: OffsetDateTime) {
    let formatted = when.format(&Rfc3339).expect("rfc3339 format");
    let conn = store.conn().expect("lock conn");
    conn.execute(
        "UPDATE entries SET created_at = ?1 WHERE id = ?2",
        params![formatted, id.to_string()],
    )
    .expect("backdate row");
}

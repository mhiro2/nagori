//! Clear-history latency harness: how long "the history disappears" takes on a
//! large corpus, split into the two phases the UX cares about separately.
//!
//! * `hard_delete` — today's `clear_non_pinned()`: one transaction that deletes
//!   every non-pinned row, cascading into representations / blobs / embeddings /
//!   thumbnails / FTS + ngram index, with `secure_delete` zeroing every freed
//!   page, then `wal_checkpoint(TRUNCATE)`. Nothing disappears from the palette
//!   until this commits.
//! * `tombstone` — the proposed "hide now" step: a single `UPDATE` that stamps
//!   `deleted_at` on every non-pinned row. Every live query already filters
//!   `deleted_at IS NULL`, so this is what the user waits for.
//! * `purge` — the deferred reclaim (`purge_deleted()`), moved off the
//!   interactive path by the split.
//!
//! Run with `cargo bench -p nagori-storage --bench clear_history`.
//! `NAGORI_CLEAR_BENCH_SIZE=20000` shrinks the corpus (default 100k).

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use nagori_core::{EntryFactory, EntryRepository};
use nagori_storage::SqliteStore;
use rusqlite::Connection;
use tokio::runtime::{Builder, Runtime};

fn gen_text(idx: usize) -> String {
    let salt = match idx % 1000 {
        0 => "needle",
        1 => "alpha",
        _ => "filler",
    };
    format!("entry-{idx:08} {salt} the quick brown fox jumps over the lazy dog {idx:04x}")
}

fn populate(store: &SqliteStore, n: usize, rt: &Runtime) {
    rt.block_on(async {
        for idx in 0..n {
            let entry = EntryFactory::from_text(gen_text(idx));
            let id = store.insert(entry).await.expect("insert");
            // ~1% pinned, matching the "curated snippets survive a clear"
            // shape the real corpus has.
            if idx.is_multiple_of(100) {
                store.set_pinned(id, true).await.expect("pin");
            }
            if (idx + 1).is_multiple_of(10_000) {
                eprintln!("    populated {}/{n}", idx + 1);
                let _ = std::io::stderr().flush();
            }
        }
    });
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[allow(clippy::cast_precision_loss)]
fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn db_size_bytes(path: &Path) -> u64 {
    let mut total = std::fs::metadata(path).map_or(0, |m| m.len());
    for suffix in ["-wal", "-shm"] {
        let side = format!("{}{suffix}", path.display());
        total += std::fs::metadata(&side).map_or(0, |m| m.len());
    }
    total
}

fn report(label: &str, elapsed: Duration, rows: usize, path: &Path) {
    println!(
        "  {label:<12} {:>10.1}ms  rows={rows:>8}  db={:>8.1}MiB",
        ms(elapsed),
        mib(db_size_bytes(path)),
    );
    let _ = std::io::stdout().flush();
}

/// Copy a quiesced database file so two variants measure the same corpus.
fn snapshot_db(src: &Path, dst: &Path) {
    {
        let conn = Connection::open(src).expect("open for checkpoint");
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint");
    }
    std::fs::copy(src, dst).expect("copy db");
}

fn main() {
    let rt = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let size: usize = std::env::var("NAGORI_CLEAR_BENCH_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);

    println!("clear-history latency harness — size={size}");
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("clear.db");
    let baseline_path = dir.path().join("clear_baseline.db");

    eprintln!("populating size={size} ...");
    let populate_start = Instant::now();
    {
        let store = SqliteStore::open(&db_path).expect("open store");
        populate(&store, size, &rt);
    }
    eprintln!(
        "populated in {:.1}s",
        populate_start.elapsed().as_secs_f64()
    );

    snapshot_db(&db_path, &baseline_path);
    println!(
        "  {:<12} {:>10}   db={:>8.1}MiB",
        "seeded",
        "-",
        mib(db_size_bytes(&db_path))
    );

    // Baseline: today's single-transaction hard delete.
    {
        let store = SqliteStore::open(&baseline_path).expect("open baseline");
        let start = Instant::now();
        let purged = rt.block_on(async { store.clear_non_pinned().await.expect("clear") });
        report("hard_delete", start.elapsed(), purged, &baseline_path);
    }

    // Proposed phase 1: stamp `deleted_at` on every non-pinned live row.
    {
        let conn = Connection::open(&db_path).expect("open for tombstone");
        let start = Instant::now();
        let changed = conn
            .execute(
                "UPDATE entries SET deleted_at = '2026-01-01T00:00:00Z'
                 WHERE pinned = 0 AND deleted_at IS NULL",
                [],
            )
            .expect("tombstone");
        report("tombstone", start.elapsed(), changed, &db_path);
    }

    // Proposed phase 2: the deferred reclaim, off the interactive path.
    {
        let store = SqliteStore::open(&db_path).expect("reopen store");
        let start = Instant::now();
        let purged = rt.block_on(async { store.purge_deleted().await.expect("purge") });
        report("purge", start.elapsed(), purged, &db_path);
    }
}

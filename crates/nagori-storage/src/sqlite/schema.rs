use nagori_core::{AppError, Result};
use rusqlite::Connection;

use super::convert::storage_err;

/// Ordered list of schema migrations.
///
/// Each entry is `(target_version, sql)`. `target_version` must be strictly
/// greater than the previous entry's, contiguous, and monotonic. Once the
/// project ships, future schema changes must append a new migration rather
/// than editing the existing entry — partial application is gated on
/// `user_version` so renumbering would silently re-run statements on
/// already-migrated databases. Pre-release we keep a single consolidated
/// migration so a fresh install sees the final shape directly.
///
/// The pre-release schema lives at `user_version = 100` so any
/// pre-existing dev database at a pre-consolidation version (the
/// legacy 1..=5 line, or any other value below the first migration)
/// trips the explicit pre-consolidation guard in [`run_migrations`]
/// and fails loud at startup rather than silently running the new
/// code against an old shape. Operators are expected to delete the
/// local DB and let it be recreated on next launch.
pub(crate) const MIGRATIONS: &[(i64, &str)] = &[(100, SCHEMA_V1), (101, ADD_NGRAM_INDEX_VERSION)];

/// Highest schema version supported by this binary. A DB whose
/// `user_version` already exceeds this is from a newer build and we refuse
/// to run against it rather than silently downgrade.
pub(crate) const SCHEMA_VERSION: i64 = const_max_version(MIGRATIONS);

const fn const_max_version(list: &[(i64, &str)]) -> i64 {
    let mut idx = 0;
    let mut max = 0;
    while idx < list.len() {
        if list[idx].0 > max {
            max = list[idx].0;
        }
        idx += 1;
    }
    max
}

pub(crate) fn run_migrations(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("SELECT user_version FROM pragma_user_version", [], |row| {
            row.get(0)
        })
        .map_err(|err| storage_err(&err))?;
    if current > SCHEMA_VERSION {
        return Err(AppError::Storage(format!(
            "database schema version {current} is newer than this build supports ({SCHEMA_VERSION}); refusing to open",
        )));
    }
    // Fresh databases sit at `user_version = 0`. Pre-release we keep a
    // single consolidated migration at version 100, so a fresh install
    // bootstraps directly to that version. Any DB at a pre-consolidation
    // version (`0 < current < first_version`) is a stale dev DB whose
    // shape predates the consolidated schema — fail loud rather than
    // silently re-running the CREATE TABLE IF NOT EXISTS statements
    // against a structurally different table.
    let first_version = MIGRATIONS.first().map_or(0, |(version, _)| *version);
    if current > 0 && current < first_version {
        return Err(AppError::Storage(format!(
            "database schema version {current} predates the consolidated pre-release schema ({first_version}); delete the local DB and let it be recreated",
        )));
    }
    let mut last_applied = if current == 0 {
        first_version - 1
    } else {
        current
    };
    for (version, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        if *version != last_applied + 1 {
            return Err(AppError::Storage(format!(
                "schema migrations are non-contiguous: jumped from {last_applied} to {version}",
            )));
        }
        let tx = conn.transaction().map_err(|err| storage_err(&err))?;
        // Concatenate the version bump onto the migration SQL so a
        // single `execute_batch` runs both as one unit. This guarantees
        // the version pragma can never execute without the preceding
        // schema statements succeeding — even if a future refactor
        // accidentally splits the transaction wrapper or skips the
        // explicit `tx.commit` below. `PRAGMA user_version = ?` must be
        // a literal (it can't be bound), and `version` comes from the
        // hard-coded `MIGRATIONS` table, so inlining is safe.
        let stamped = format!("{sql}\nPRAGMA user_version = {version};");
        tx.execute_batch(&stamped)
            .map_err(|err| storage_err(&err))?;
        tx.commit().map_err(|err| storage_err(&err))?;
        last_applied = *version;
    }
    Ok(())
}

/// Single consolidated schema. Connection-level PRAGMAs
/// (`foreign_keys`, `journal_mode`, …) are asserted in
/// `configure_connection` for every pool slot, so we don't repeat them
/// here.
///
/// Dedupe is keyed on `representation_set_hash`: two snapshots with the
/// same primary text but different HTML/RTF/file-list alternatives hash
/// to different values and therefore land in different rows, so the
/// later copy can't silently overwrite the earlier row's alternatives.
/// Entries built without a `pending_representations` set (CLI `add_text`,
/// synthesised rows) fall back to `content_hash` for this column at
/// insert time so identical-primary inserts still collide.
///
/// `search_fts` is an external-content FTS5 over `search_documents`,
/// with sync triggers below. The previous shape stored `entry_id` as an
/// UNINDEXED column, which forced per-entry deletes to scan every
/// posting list; external content keys FTS rows by source `rowid`, so
/// the trigger-driven deletes are rowid-equality lookups.
///
/// `entries.total_byte_count` is maintained by triggers on
/// `entry_representations` so the retention / byte-budget paths can
/// read a single column instead of recomputing `SUM(byte_count)` over
/// the dependent table on every pass.
const SCHEMA_V1: &str = r"
CREATE TABLE IF NOT EXISTS entries (
    id TEXT PRIMARY KEY,
    content_kind TEXT NOT NULL
        CHECK (content_kind IN (
            'text', 'url', 'code', 'image', 'file_list', 'rich_text', 'unknown'
        )),
    content_json TEXT NOT NULL,
    source_app_name TEXT,
    source_bundle_id TEXT,
    source_executable_path TEXT,
    content_hash TEXT NOT NULL,
    representation_set_hash TEXT NOT NULL,
    sensitivity TEXT NOT NULL
        CHECK (sensitivity IN (
            'unknown', 'public', 'private', 'secret', 'blocked'
        )),
    pinned INTEGER NOT NULL DEFAULT 0
        CHECK (pinned IN (0, 1)),
    archived INTEGER NOT NULL DEFAULT 0
        CHECK (archived IN (0, 1)),
    use_count INTEGER NOT NULL DEFAULT 0
        CHECK (use_count >= 0),
    -- Materialised total of `entry_representations.byte_count` for this
    -- entry. Updated by triggers on the representation table so the
    -- retention / byte-budget paths read a single column instead of
    -- joining + summing each pass.
    total_byte_count INTEGER NOT NULL DEFAULT 0
        CHECK (total_byte_count >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_used_at TEXT,
    expires_at TEXT,
    deleted_at TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_representation_set_hash
    ON entries(representation_set_hash)
    WHERE deleted_at IS NULL;

-- `recent_entries` with RecentOrder::ByRecency walks `created_at DESC`
-- alone; a `(pinned, created_at)` composite can't satisfy that without
-- a sort.
CREATE INDEX IF NOT EXISTS idx_entries_live_created_at
    ON entries(created_at DESC)
    WHERE deleted_at IS NULL AND sensitivity != 'blocked';

-- `recent_entries` with PinnedFirstThenRecency and the
-- `recent_live` CTE in `substring_candidates` both order by
-- (pinned DESC, created_at DESC). A composite over live, non-blocked
-- rows lets the planner walk the index forward and stop after LIMIT.
CREATE INDEX IF NOT EXISTS idx_entries_recent_live
    ON entries(pinned DESC, created_at DESC)
    WHERE deleted_at IS NULL AND sensitivity != 'blocked';

-- `recent_entries` with RecentOrder::ByUseCount orders by
-- (use_count DESC, COALESCE(last_used_at, created_at) DESC, created_at DESC).
-- The expression key matches the ORDER BY exactly so the index can be
-- walked forward without a sort step.
CREATE INDEX IF NOT EXISTS idx_entries_use_count_live
    ON entries(
        use_count DESC,
        COALESCE(last_used_at, created_at) DESC,
        created_at DESC
    )
    WHERE deleted_at IS NULL AND sensitivity != 'blocked';

-- `list_pinned` orders pinned-only live rows by `updated_at DESC`.
-- Pinned rows get pin-toggled or relabelled long after creation, so
-- `updated_at` (not `created_at`) is the ordering the UI wants.
CREATE INDEX IF NOT EXISTS idx_entries_pinned_updated_live
    ON entries(updated_at DESC)
    WHERE pinned = 1 AND deleted_at IS NULL AND sensitivity != 'blocked';

-- Retention / byte-budget candidate selection over unpinned live rows.
-- `enforce_retention_count` walks DESC + OFFSET; `enforce_total_bytes`
-- walks ASC (oldest first). A single DESC index covers both because
-- SQLite can walk it in reverse for the ASC case.
CREATE INDEX IF NOT EXISTS idx_entries_unpinned_live
    ON entries(created_at DESC)
    WHERE deleted_at IS NULL AND pinned = 0;

-- UI filter shortcuts. The desktop palette filters by content_kind and
-- source_app on every keystroke; without the partial indexes those
-- branches fall back to scanning the live partition.
CREATE INDEX IF NOT EXISTS idx_entries_content_kind_live
    ON entries(content_kind, created_at DESC)
    WHERE deleted_at IS NULL AND sensitivity != 'blocked';

CREATE INDEX IF NOT EXISTS idx_entries_source_bundle_live
    ON entries(source_bundle_id, created_at DESC)
    WHERE deleted_at IS NULL
      AND sensitivity != 'blocked'
      AND source_bundle_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_entries_source_app_name_live
    ON entries(source_app_name, created_at DESC)
    WHERE deleted_at IS NULL
      AND sensitivity != 'blocked'
      AND source_app_name IS NOT NULL;

CREATE TABLE IF NOT EXISTS entry_representations (
    id TEXT PRIMARY KEY,
    entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    role TEXT NOT NULL
        CHECK (role IN ('primary', 'plain_fallback', 'alternative')),
    mime_type TEXT NOT NULL,
    platform_format TEXT,
    ordinal INTEGER NOT NULL
        CHECK (ordinal >= 0),
    text_content TEXT,
    payload_blob BLOB,
    byte_count INTEGER NOT NULL
        CHECK (byte_count >= 0),
    created_at TEXT NOT NULL,
    -- Each row carries exactly one of text_content or payload_blob.
    -- The capture pipeline relies on this to pick the right preview
    -- path without re-inspecting the bytes.
    CHECK (
        (text_content IS NOT NULL AND payload_blob IS NULL)
     OR (text_content IS NULL AND payload_blob IS NOT NULL)
    ),
    UNIQUE (entry_id, role, ordinal)
);

CREATE INDEX IF NOT EXISTS idx_entry_representations_entry
    ON entry_representations(entry_id, ordinal);

-- Maintain `entries.total_byte_count` for the retention path. The
-- triggers handle every byte_count delta path: new rep, dropped rep,
-- replaced rep (UPDATE OF byte_count or entry_id).
CREATE TRIGGER IF NOT EXISTS entry_representations_ai_total
AFTER INSERT ON entry_representations
BEGIN
    UPDATE entries
       SET total_byte_count = total_byte_count + NEW.byte_count
     WHERE id = NEW.entry_id;
END;

CREATE TRIGGER IF NOT EXISTS entry_representations_ad_total
AFTER DELETE ON entry_representations
BEGIN
    UPDATE entries
       SET total_byte_count = total_byte_count - OLD.byte_count
     WHERE id = OLD.entry_id;
END;

CREATE TRIGGER IF NOT EXISTS entry_representations_au_total
AFTER UPDATE OF byte_count, entry_id ON entry_representations
BEGIN
    UPDATE entries
       SET total_byte_count = total_byte_count - OLD.byte_count
     WHERE id = OLD.entry_id;
    UPDATE entries
       SET total_byte_count = total_byte_count + NEW.byte_count
     WHERE id = NEW.entry_id;
END;

CREATE TABLE IF NOT EXISTS search_documents (
    -- Explicit `INTEGER PRIMARY KEY` aliases the SQLite rowid into a
    -- stable column. `VACUUM` is documented to renumber rowids of
    -- tables without an INTEGER PRIMARY KEY, which would invalidate
    -- the FTS5 external-content pointer (`content_rowid = 'doc_id'`)
    -- and silently corrupt search hits. Pinning the rowid to `doc_id`
    -- keeps `search_fts` consistent across `VACUUM`.
    doc_id INTEGER PRIMARY KEY,
    entry_id TEXT NOT NULL UNIQUE REFERENCES entries(id) ON DELETE CASCADE,
    title TEXT,
    preview TEXT NOT NULL,
    normalized_text TEXT NOT NULL,
    language TEXT
);

CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
    title,
    preview,
    normalized_text,
    content = 'search_documents',
    content_rowid = 'doc_id',
    tokenize = 'unicode61'
);

CREATE TRIGGER IF NOT EXISTS search_documents_ai_fts
AFTER INSERT ON search_documents
BEGIN
    INSERT INTO search_fts(rowid, title, preview, normalized_text)
    VALUES (NEW.doc_id, NEW.title, NEW.preview, NEW.normalized_text);
END;

CREATE TRIGGER IF NOT EXISTS search_documents_ad_fts
AFTER DELETE ON search_documents
BEGIN
    INSERT INTO search_fts(search_fts, rowid, title, preview, normalized_text)
    VALUES ('delete', OLD.doc_id, OLD.title, OLD.preview, OLD.normalized_text);
END;

CREATE TRIGGER IF NOT EXISTS search_documents_au_fts
AFTER UPDATE ON search_documents
BEGIN
    INSERT INTO search_fts(search_fts, rowid, title, preview, normalized_text)
    VALUES ('delete', OLD.doc_id, OLD.title, OLD.preview, OLD.normalized_text);
    INSERT INTO search_fts(rowid, title, preview, normalized_text)
    VALUES (NEW.doc_id, NEW.title, NEW.preview, NEW.normalized_text);
END;

CREATE TABLE IF NOT EXISTS ngrams (
    gram TEXT NOT NULL,
    entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    position INTEGER NOT NULL
        CHECK (position >= 0),
    PRIMARY KEY (gram, entry_id, position)
);

-- `idx_ngrams_gram_entry` is for the ngram fan-out: `WHERE n.gram IN (…)`
-- then `GROUP BY entry_id`. `idx_ngrams_entry_id` is for per-entry
-- deletes (the soft-delete prune path).
CREATE INDEX IF NOT EXISTS idx_ngrams_gram_entry ON ngrams(gram, entry_id);
CREATE INDEX IF NOT EXISTS idx_ngrams_entry_id ON ngrams(entry_id);

CREATE TABLE IF NOT EXISTS entry_thumbnails (
    entry_id TEXT PRIMARY KEY REFERENCES entries(id) ON DELETE CASCADE,
    payload_blob BLOB NOT NULL,
    mime_type TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    byte_count INTEGER NOT NULL
        CHECK (byte_count >= 0),
    created_at TEXT NOT NULL,
    last_accessed_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_entry_thumbnails_last_accessed_at
    ON entry_thumbnails(last_accessed_at);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_events (
    id TEXT PRIMARY KEY,
    event_kind TEXT NOT NULL,
    entry_id TEXT,
    message TEXT,
    created_at TEXT NOT NULL
);

-- On-device semantic search index. Vectors are stored as raw little-endian
-- float32 BLOBs (the layout `sqlite-vec`'s `vec_distance_cosine` consumes
-- directly); the dimension is whatever the embedder reports at runtime, so it
-- is recorded per row rather than baked into the schema. `ON DELETE CASCADE`
-- drops a row's embedding when its entry is hard-deleted; soft-deleted entries
-- keep their vector but are filtered out at query time. `content_hash` lets the
-- indexer skip re-embedding entries whose text is unchanged.
CREATE TABLE IF NOT EXISTS entry_embeddings (
    entry_id TEXT PRIMARY KEY REFERENCES entries(id) ON DELETE CASCADE,
    vector BLOB NOT NULL,
    dimension INTEGER NOT NULL
        CHECK (dimension > 0),
    content_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Singleton row describing the embedding model the stored vectors were
-- produced with. Compared at startup against the live embedder's metadata so a
-- model / revision / dimension change clears the index and triggers a rebuild
-- instead of mixing incompatible embedding spaces.
CREATE TABLE IF NOT EXISTS semantic_index_meta (
    id INTEGER PRIMARY KEY
        CHECK (id = 1),
    model_identifier TEXT NOT NULL,
    revision INTEGER NOT NULL,
    dimension INTEGER NOT NULL,
    max_sequence_length INTEGER NOT NULL,
    languages TEXT NOT NULL,
    index_version INTEGER NOT NULL,
    updated_at TEXT NOT NULL
);
";

/// Per-row marker recording which ngram-generator revision built a document's
/// grams. Existing rows default to `0` (stale) so the background rebuild worker
/// regenerates them once after an upgrade that changes the generator (kana
/// folding, Han 1-grams); fresh captures stamp the current revision in the same
/// transaction as the grams, so they are never re-processed. The
/// `(ngram_index_version, doc_id)` index makes "fetch the next stale batch" an
/// index range scan. Appended as migration 101 rather than edited into
/// `SCHEMA_V1` so it reaches pre-release databases already at `user_version =
/// 100`.
const ADD_NGRAM_INDEX_VERSION: &str = r"
ALTER TABLE search_documents ADD COLUMN ngram_index_version INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_search_documents_ngram_version_doc_id
    ON search_documents(ngram_index_version, doc_id);
";

use nagori_core::{EntryFactory, EntryRepository, SearchFilters, SearchQuery, Sensitivity};
use nagori_search::{MAX_NGRAM_INPUT_CHARS, normalize_text};
use rusqlite::params;
use time::OffsetDateTime;

use super::super::*;

use super::{backdate_entry, insert_text};

#[tokio::test]
async fn duplicate_insert_returns_existing_id() {
    let store = SqliteStore::open_memory().unwrap();
    let first_id = insert_text(&store, "same clipboard value").await;
    let second_id = insert_text(&store, "same clipboard value").await;

    assert_eq!(second_id, first_id);
    let entries = store.list_recent(10).await.unwrap();
    assert_eq!(entries.len(), 1);

    let query = SearchQuery::new("clipboard", normalize_text("clipboard"), 10);
    let results = store.search(query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry_id, first_id);
}

#[tokio::test]
async fn duplicate_insert_with_identical_reps_refreshes_source() {
    // Dedupe is keyed on `representation_set_hash`, so two snapshots
    // with the same primary AND the same alternatives collide. The
    // dedupe path must then refresh the entries row's source columns
    // and bump `created_at`/`updated_at` so the source_app filter sees
    // the latest copy — not the first one.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, SourceApp,
    };

    let store = SqliteStore::open_memory().unwrap();

    let make_snapshot = |bundle: &str| ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("dedupe-rewrite"),
        captured_at: OffsetDateTime::now_utc(),
        source: Some(SourceApp {
            bundle_id: Some(bundle.to_owned()),
            name: Some(bundle.to_owned()),
            executable_path: None,
        }),
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>shared</p>".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("shared body".to_owned()),
            },
        ],
    };

    let first =
        EntryFactory::from_snapshot(make_snapshot("com.example.editor")).expect("first snapshot");
    let first_id = store.insert(first).await.unwrap();

    let second = EntryFactory::from_snapshot(make_snapshot("com.example.terminal"))
        .expect("second snapshot");
    let second_id = store.insert(second).await.unwrap();
    assert_eq!(second_id, first_id, "dedupe should reuse the row");

    let fetched = store.get(first_id).await.unwrap().expect("row exists");
    let source = fetched.metadata.source.as_ref().expect("source preserved");
    assert_eq!(source.bundle_id.as_deref(), Some("com.example.terminal"));

    let mut query = SearchQuery::new("shared", normalize_text("shared"), 10);
    query.filters = SearchFilters {
        source_app: Some("com.example.terminal".to_owned()),
        ..Default::default()
    };
    let hits = store.search(query).await.unwrap();
    assert_eq!(hits.len(), 1, "source filter must hit the new source");
    assert_eq!(hits[0].entry_id, first_id);
}

#[tokio::test]
async fn duplicate_insert_never_demotes_sensitivity() {
    // Rep-less entries fall back to `representation_set_hash =
    // content_hash`, so re-adding the same text through a path that never
    // classifies (CLI `add` stores `Unknown`) collides with the secret row.
    // The dedupe UPDATE must keep the more protective classification —
    // otherwise the row would re-enter default listings, search previews,
    // and the semantic-embedding/thumbnail gates.
    let store = SqliteStore::open_memory().unwrap();

    let mut secret = EntryFactory::from_text("sk-live-very-secret-token");
    secret.search.normalized_text = normalize_text(secret.plain_text().unwrap());
    secret.sensitivity = Sensitivity::Secret;
    let secret_id = store.insert(secret).await.unwrap();

    let unknown_id = insert_text(&store, "sk-live-very-secret-token").await;
    assert_eq!(unknown_id, secret_id, "dedupe should reuse the row");

    let fetched = store.get(secret_id).await.unwrap().expect("row exists");
    assert_eq!(
        fetched.sensitivity,
        Sensitivity::Secret,
        "an unclassified re-capture must not demote a secret row"
    );
}

#[tokio::test]
async fn duplicate_insert_never_restores_raw_search_document_on_secret_row() {
    // Sensitivity surviving the dedupe is not enough on its own: the search
    // document of a `Secret` row was redacted by the classifier at capture
    // time, while an unclassified re-capture carries the raw preview /
    // normalized text. The dedupe upsert must keep the redacted document —
    // `Secret` previews ship verbatim in default DTOs on the assumption
    // they are redacted, and the raw text must not become searchable.
    let store = SqliteStore::open_memory().unwrap();

    let mut secret = EntryFactory::from_text("sk-live-very-secret-token");
    secret.sensitivity = Sensitivity::Secret;
    secret.search.preview = "sk-live-[redacted]".to_owned();
    secret.search.normalized_text = normalize_text("sk-live-[redacted]");
    let secret_id = store.insert(secret).await.unwrap();

    let unknown_id = insert_text(&store, "sk-live-very-secret-token").await;
    assert_eq!(unknown_id, secret_id, "dedupe should reuse the row");

    let fetched = store.get(secret_id).await.unwrap().expect("row exists");
    assert_eq!(
        fetched.search.preview, "sk-live-[redacted]",
        "the redacted preview must survive an unclassified re-capture"
    );
    assert!(
        !fetched.search.normalized_text.contains("very-secret-token"),
        "raw text must not re-enter the search document"
    );

    let query = SearchQuery::new("very-secret-token", normalize_text("very-secret-token"), 10);
    let hits = store.search(query).await.unwrap();
    assert!(
        hits.is_empty(),
        "the raw secret must not be searchable after the dedupe"
    );
}

#[tokio::test]
async fn duplicate_insert_still_promotes_sensitivity() {
    // The monotone merge is one-way: a re-capture that classifies *more*
    // protectively (e.g. a denylist rule added after the first copy) must
    // still land on the stored row.
    let store = SqliteStore::open_memory().unwrap();

    let unknown_id = insert_text(&store, "soon to be denylisted").await;

    let mut secret = EntryFactory::from_text("soon to be denylisted");
    secret.search.normalized_text = normalize_text(secret.plain_text().unwrap());
    secret.sensitivity = Sensitivity::Secret;
    let secret_id = store.insert(secret).await.unwrap();
    assert_eq!(secret_id, unknown_id, "dedupe should reuse the row");

    let fetched = store.get(unknown_id).await.unwrap().expect("row exists");
    assert_eq!(fetched.sensitivity, Sensitivity::Secret);
}

#[tokio::test]
async fn distinct_alternatives_produce_distinct_rows() {
    // Two snapshots with the same primary text but different HTML
    // alternatives must land in distinct rows, otherwise the later
    // capture would silently overwrite the earlier row's
    // alternatives.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot, SourceApp,
    };

    let store = SqliteStore::open_memory().unwrap();

    let make_snapshot = |html: &str| ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("distinct-alts"),
        captured_at: OffsetDateTime::now_utc(),
        source: Some(SourceApp {
            bundle_id: Some("com.example.editor".to_owned()),
            name: Some("editor".to_owned()),
            executable_path: None,
        }),
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text(html.to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("shared body".to_owned()),
            },
        ],
    };

    let first = EntryFactory::from_snapshot(make_snapshot("<p>v1</p>")).expect("first snapshot");
    let first_id = store.insert(first).await.unwrap();
    let second = EntryFactory::from_snapshot(make_snapshot("<p>v2</p>")).expect("second snapshot");
    let second_id = store.insert(second).await.unwrap();

    assert_ne!(
        first_id, second_id,
        "different alternative sets must not collapse onto one row"
    );
    let first_reps = store.list_representations(first_id).await.unwrap();
    let first_html = first_reps
        .iter()
        .find(|r| r.mime_type == "text/html")
        .expect("first html rep present");
    match &first_html.data {
        nagori_core::RepresentationDataRef::InlineText(text) => {
            assert_eq!(text, "<p>v1</p>", "first row keeps its original html");
        }
        other => panic!("expected inline text rep, got {other:?}"),
    }
}

#[tokio::test]
async fn pin_round_trip() {
    let store = SqliteStore::open_memory().unwrap();
    let id = store
        .insert(EntryFactory::from_text("hello"))
        .await
        .unwrap();
    store.set_pinned(id, true).await.unwrap();
    let pinned = store.list_pinned().await.unwrap();
    assert_eq!(pinned.len(), 1);
    assert!(pinned[0].lifecycle.pinned);
}

#[tokio::test]
async fn list_pinned_excludes_blocked_rows() {
    // The capture path refuses to persist `Blocked`, but stale rows
    // from older daemons or hand-edited DBs can survive. Match
    // `list_recent` / `search` and keep them out of default lists so
    // the DTO layer never has to ship a raw-text preview from one.
    let store = SqliteStore::open_memory().unwrap();
    let pinned_public = insert_text(&store, "public pinned").await;
    store.set_pinned(pinned_public, true).await.unwrap();
    let mut blocked = EntryFactory::from_text("blocked pinned");
    blocked.search.normalized_text = normalize_text(blocked.plain_text().unwrap());
    blocked.sensitivity = Sensitivity::Blocked;
    let blocked_id = store.insert(blocked).await.unwrap();
    store.set_pinned(blocked_id, true).await.unwrap();

    let pinned = store.list_pinned().await.unwrap();
    assert_eq!(pinned.len(), 1);
    assert_eq!(pinned[0].id, pinned_public);
}

async fn audit_kind_count(store: &SqliteStore, kind: &str) -> i64 {
    store.audit_event_count(kind).await.expect("audit count")
}

#[tokio::test]
async fn insert_records_ngram_truncated_when_input_exceeds_cap() {
    // The ngram index silently caps at `MAX_NGRAM_INPUT_CHARS` so a paste
    // larger than the cap loses fuzzy-search recall on its tail. The
    // user-visible symptom — "search misses the bottom of my pasted
    // doc" — was previously invisible at the DB layer; this audit event
    // is the only artefact that survives log rotation and lets a future
    // support investigation correlate "missing matches" with the
    // specific entry that was truncated.
    let store = SqliteStore::open_memory().unwrap();
    let oversized: String = "a".repeat(MAX_NGRAM_INPUT_CHARS + 1);
    let _ = insert_text(&store, &oversized).await;

    assert_eq!(audit_kind_count(&store, "ngram_truncated").await, 1);
}

#[tokio::test]
async fn insert_skips_audit_when_input_fits_cap() {
    // Negative case: an entry that fits inside the cap must not emit an
    // audit row, otherwise the events table fills up with noise on
    // every paste and obscures the genuine truncation signal.
    let store = SqliteStore::open_memory().unwrap();
    let _ = insert_text(&store, "a short paste").await;

    assert_eq!(audit_kind_count(&store, "ngram_truncated").await, 0);
}

#[tokio::test]
async fn retention_delete_prunes_search_tables() {
    let store = SqliteStore::open_memory().unwrap();
    let _ = insert_text(&store, "temporary searchable value").await;

    let deleted = store
        .clear_older_than(OffsetDateTime::now_utc() + time::Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(deleted, 1);

    let conn = store.conn().unwrap();
    for table in ["search_documents", "search_fts", "ngrams"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "{table} should be pruned");
    }
}

fn count_active(store: &SqliteStore) -> i64 {
    let conn = store.conn().expect("lock conn");
    conn.query_row(
        "SELECT COUNT(*) FROM entries WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )
    .expect("count active")
}

fn count_total(store: &SqliteStore) -> i64 {
    let conn = store.conn().expect("lock conn");
    conn.query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .expect("count total")
}

#[tokio::test]
async fn enforce_retention_count_drops_oldest_unpinned() {
    let store = SqliteStore::open_memory().unwrap();
    let now = OffsetDateTime::now_utc();
    let oldest = insert_text(&store, "oldest entry").await;
    let middle = insert_text(&store, "middle entry").await;
    let newest = insert_text(&store, "newest entry").await;
    backdate_entry(&store, oldest, now - time::Duration::days(3));
    backdate_entry(&store, middle, now - time::Duration::days(2));
    backdate_entry(&store, newest, now - time::Duration::days(1));

    let removed = store.enforce_retention_count(2).await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(count_active(&store), 2);

    let surviving = store
        .list_recent(10)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert!(surviving.contains(&middle));
    assert!(surviving.contains(&newest));
    assert!(!surviving.contains(&oldest));

    // Idempotent: a second call with the same cap removes nothing.
    assert_eq!(store.enforce_retention_count(2).await.unwrap(), 0);
}

#[tokio::test]
async fn enforce_retention_count_hard_deletes_evicted_rows() {
    // Retention eviction must *physically* remove the row — and its
    // representations / search index via `ON DELETE CASCADE` — rather than
    // tombstone it. A soft delete left the body, blobs, and embeddings on
    // disk, so a retention cap never reclaimed space and the content stayed
    // recoverable from the file.
    let store = SqliteStore::open_memory().unwrap();
    let now = OffsetDateTime::now_utc();
    let oldest = insert_text(&store, "oldest entry").await;
    let _middle = insert_text(&store, "middle entry").await;
    let _newest = insert_text(&store, "newest entry").await;
    backdate_entry(&store, oldest, now - time::Duration::days(3));

    assert_eq!(count_total(&store), 3);
    let removed = store.enforce_retention_count(2).await.unwrap();
    assert_eq!(removed, 1);

    // The evicted row is gone from the table entirely, not just filtered
    // out by `deleted_at`.
    assert_eq!(count_total(&store), 2);
    assert_eq!(count_active(&store), 2);
    let conn = store.conn().expect("lock conn");
    let surviving: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entries WHERE id = ?1",
            params![oldest.to_string()],
            |row| row.get(0),
        )
        .expect("count evicted row");
    assert_eq!(surviving, 0, "evicted row must be physically deleted");
    // Its representation rows cascade away with it.
    let reps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry_representations WHERE entry_id = ?1",
            params![oldest.to_string()],
            |row| row.get(0),
        )
        .expect("count evicted representations");
    assert_eq!(
        reps, 0,
        "evicted row's representations must cascade-delete with it",
    );
}

#[tokio::test]
async fn enforce_retention_count_keeps_pinned_above_cap() {
    // Pinned entries never count toward the eviction window: the
    // OFFSET-based delete only sees unpinned rows, so a single pinned
    // ancient row plus N unpinned rows yields exactly N retained.
    let store = SqliteStore::open_memory().unwrap();
    let now = OffsetDateTime::now_utc();
    let pinned_old = insert_text(&store, "pinned ancient").await;
    let oldest = insert_text(&store, "regular oldest").await;
    let middle = insert_text(&store, "regular middle").await;
    let newest = insert_text(&store, "regular newest").await;
    backdate_entry(&store, pinned_old, now - time::Duration::days(10));
    backdate_entry(&store, oldest, now - time::Duration::days(3));
    backdate_entry(&store, middle, now - time::Duration::days(2));
    backdate_entry(&store, newest, now - time::Duration::days(1));
    store.set_pinned(pinned_old, true).await.unwrap();

    let removed = store.enforce_retention_count(1).await.unwrap();
    assert_eq!(removed, 2);

    let active_ids = store
        .list_recent(10)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert!(active_ids.contains(&pinned_old), "pinned must survive");
    assert!(active_ids.contains(&newest), "newest unpinned must survive");
    assert!(!active_ids.contains(&middle));
    assert!(!active_ids.contains(&oldest));
}

#[tokio::test]
async fn clear_older_than_skips_pinned() {
    let store = SqliteStore::open_memory().unwrap();
    let now = OffsetDateTime::now_utc();
    let pinned = insert_text(&store, "pinned ancient").await;
    let stale = insert_text(&store, "stale ancient").await;
    let fresh = insert_text(&store, "fresh value").await;
    backdate_entry(&store, pinned, now - time::Duration::days(40));
    backdate_entry(&store, stale, now - time::Duration::days(40));
    backdate_entry(&store, fresh, now - time::Duration::days(1));
    store.set_pinned(pinned, true).await.unwrap();

    let removed = store
        .clear_older_than(now - time::Duration::days(7))
        .await
        .unwrap();
    assert_eq!(removed, 1);

    let surviving = store
        .list_recent(10)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert!(surviving.contains(&pinned), "pinned should survive cutoff");
    assert!(surviving.contains(&fresh), "fresh row must remain");
    assert!(!surviving.contains(&stale), "stale row should be cleared");
}

#[tokio::test]
async fn clear_non_pinned_purges_only_unpinned_rows() {
    let store = SqliteStore::open_memory().unwrap();
    let pinned = insert_text(&store, "pinned anchor").await;
    let unpinned_a = insert_text(&store, "ephemeral one").await;
    let unpinned_b = insert_text(&store, "ephemeral two").await;
    store.set_pinned(pinned, true).await.unwrap();

    let removed = store.clear_non_pinned().await.unwrap();
    assert_eq!(removed, 2);

    let surviving = store
        .list_recent(10)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert_eq!(surviving, vec![pinned], "only pinned row must survive");
    assert!(!surviving.contains(&unpinned_a));
    assert!(!surviving.contains(&unpinned_b));
}

#[tokio::test]
async fn clear_non_pinned_hard_deletes_unpinned_content() {
    // "Clear history" / clear-on-quit must physically purge non-pinned
    // rows (body, representations, search index) so nothing is
    // recoverable from the live table — while the pinned row keeps its
    // content intact.
    let store = SqliteStore::open_memory().unwrap();
    let pinned = insert_text(&store, "pinned anchor").await;
    let unpinned = insert_text(&store, "ephemeral secret").await;
    store.set_pinned(pinned, true).await.unwrap();

    let removed = store.clear_non_pinned().await.unwrap();
    assert_eq!(removed, 1);

    // Only the pinned row remains anywhere in the table.
    assert_eq!(count_total(&store), 1);
    let conn = store.conn().unwrap();
    for (table, column) in [
        ("entries", "id"),
        ("entry_representations", "entry_id"),
        ("search_documents", "entry_id"),
        ("ngrams", "entry_id"),
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
        let count: i64 = conn
            .query_row(&sql, params![unpinned.to_string()], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table} rows for the cleared entry must be gone");
    }
    // The pinned row's content survives.
    let pinned_reps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry_representations WHERE entry_id = ?1",
            params![pinned.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(pinned_reps > 0, "pinned row must keep its representations");
}

#[tokio::test]
async fn purge_deleted_hard_deletes_tombstones_including_pinned() {
    // `mark_deleted` only tombstones; `purge_deleted` is the deferred
    // reclaim. It must physically drop *every* tombstoned row — including a
    // pinned one, which no `pinned = 0` retention path would ever reach —
    // while leaving a live (non-deleted) row untouched. Without it a
    // "delete this pinned secret" would keep its body/blobs on disk forever.
    let store = SqliteStore::open_memory().unwrap();
    let pinned_deleted = insert_text(&store, "pinned secret to delete").await;
    let plain_deleted = insert_text(&store, "ordinary deleted").await;
    let live = insert_text(&store, "still here").await;
    store.set_pinned(pinned_deleted, true).await.unwrap();
    store.mark_deleted(pinned_deleted).await.unwrap();
    store.mark_deleted(plain_deleted).await.unwrap();

    // Soft delete leaves the rows on disk, just hidden from live queries.
    assert_eq!(count_total(&store), 3);
    assert_eq!(count_active(&store), 1);

    let purged = store.purge_deleted().await.unwrap();
    assert_eq!(
        purged, 2,
        "both tombstones (incl. the pinned one) must be reclaimed",
    );

    // Only the live row remains anywhere in the table, with its content.
    assert_eq!(count_total(&store), 1);
    assert_eq!(count_active(&store), 1);
    let conn = store.conn().unwrap();
    for id in [pinned_deleted, plain_deleted] {
        for (table, column) in [
            ("entries", "id"),
            ("entry_representations", "entry_id"),
            ("search_documents", "entry_id"),
            ("ngrams", "entry_id"),
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
            let count: i64 = conn
                .query_row(&sql, params![id.to_string()], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} rows for the purged entry must be gone");
        }
    }
    let live_reps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM entry_representations WHERE entry_id = ?1",
            params![live.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(live_reps > 0, "the live row must keep its representations");
    drop(conn);

    // Idempotent: a second purge with no tombstones removes nothing.
    assert_eq!(store.purge_deleted().await.unwrap(), 0);
}

#[tokio::test]
async fn reinserting_after_mark_deleted_creates_new_row() {
    // The content-hash UNIQUE index is `WHERE deleted_at IS NULL`, so
    // tombstoned rows must not block re-inserts of the same text.
    let store = SqliteStore::open_memory().unwrap();
    let original = insert_text(&store, "duplicated value").await;
    store.mark_deleted(original).await.unwrap();
    assert_eq!(count_active(&store), 0);

    let revived = insert_text(&store, "duplicated value").await;
    assert_ne!(
        revived, original,
        "soft-deleted hash must not be reused as the live id",
    );

    // Tombstone is preserved alongside the new active row.
    assert_eq!(count_active(&store), 1);
    assert_eq!(count_total(&store), 2);

    // The fresh row owns the search artefacts and is queryable.
    let query = SearchQuery::new("duplicated", normalize_text("duplicated"), 10);
    let results = store.search(query).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].entry_id, revived);
}

#[tokio::test]
async fn trim_alternatives_drops_oversized_alts_before_insert() {
    // Mirror the capture pipeline's budget enforcement at the storage
    // boundary: feed an entry whose primary fits but whose alternatives
    // would blow past `max_total_bytes`, trim it, and confirm the only
    // rows that land in SQLite are the ones that survived the trim. The
    // recomputed `representation_set_hash` keeps dedupe honest about
    // what storage actually wrote.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
        factory::compute_representation_set_hash,
    };

    let big_rtf = "{\\rtf1 ".to_owned() + &"a".repeat(2048) + "}";
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("trim-test"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>hi</p>".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("hi".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "application/rtf".to_owned(),
                data: ClipboardData::Text(big_rtf),
            },
        ],
    };
    let mut entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
    let trimmed = entry.trim_alternatives_to_budget(64);
    assert!(trimmed, "RTF alternative should be trimmed");
    entry.metadata.representation_set_hash = Some(compute_representation_set_hash(
        &entry.pending_representations,
    ));

    let store = SqliteStore::open_memory().unwrap();
    let id = store.insert(entry).await.unwrap();

    let conn = store.conn().unwrap();
    let mime_types: Vec<String> = conn
        .prepare(
            "SELECT mime_type FROM entry_representations
             WHERE entry_id = ?1 ORDER BY ordinal ASC",
        )
        .unwrap()
        .query_map([id.to_string()], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(mime_types, vec!["text/html", "text/plain"]);
}

#[tokio::test]
async fn duplicate_live_insert_does_not_duplicate_search_rows() {
    let store = SqliteStore::open_memory().unwrap();
    let first = insert_text(&store, "deduped once").await;
    let again = insert_text(&store, "deduped once").await;
    assert_eq!(first, again);

    let conn = store.conn().unwrap();
    for table in ["search_documents", "search_fts"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = conn.query_row(&sql, [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1, "{table} should only hold one row per live entry");
    }
}

/// Retention hard-deletes must leave nothing recoverable in the WAL
/// sidecar. `secure_delete` zeroes the freed pages in the main file, but
/// the pre-deletion content also lives in the historical WAL frames
/// written before the delete, and a passive autocheckpoint neither
/// truncates the WAL nor guarantees those frames are gone. The purge
/// contract (`checkpoint_truncate_after_purge`) therefore requires every
/// hard-delete path — retention included — to follow up with
/// `wal_checkpoint(TRUNCATE)`, which shrinks the sidecar to zero.
#[tokio::test]
async fn enforce_retention_count_truncates_wal_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    let store = SqliteStore::open(&db_path).unwrap();
    for index in 0..4 {
        insert_text(&store, &format!("retention wal row {index}")).await;
    }
    let wal_path = temp.path().join("nagori.sqlite-wal");
    assert!(
        wal_path.metadata().unwrap().len() > 0,
        "inserts must have written WAL frames for the truncate assertion to mean anything"
    );

    let removed = store.enforce_retention_count(1).await.unwrap();
    assert_eq!(removed, 3);
    assert_eq!(
        wal_path.metadata().unwrap().len(),
        0,
        "retention purge must checkpoint-truncate the WAL sidecar"
    );
}

/// The byte-budget purge selects eviction candidates in bounded rounds
/// (`TOTAL_BYTES_EVICTION_BATCH` oldest rows at a time) instead of loading
/// every live, unpinned row id into memory inside the write lock. Verify a
/// backlog larger than one round is still drained completely and that
/// pinned rows survive — the loop must terminate on "nothing evictable
/// left" rather than spinning when only pinned rows remain over budget.
#[tokio::test]
async fn enforce_total_bytes_drains_backlog_across_eviction_rounds() {
    use super::super::maintenance::TOTAL_BYTES_EVICTION_BATCH;

    let store = SqliteStore::open_memory().unwrap();
    let backlog = usize::try_from(TOTAL_BYTES_EVICTION_BATCH).unwrap() + 5;
    for index in 0..backlog {
        insert_text(&store, &format!("eviction round row {index}")).await;
    }
    let pinned_id = insert_text(&store, "pinned survivor").await;
    store.set_pinned(pinned_id, true).await.unwrap();

    // A zero budget forces eviction of every unpinned row, which takes
    // more than one candidate round; the pinned row keeps the total
    // above budget, so the loop must still stop once only pinned rows
    // are left.
    let deleted = store.enforce_total_bytes(0).await.unwrap();
    assert_eq!(deleted, backlog, "every unpinned row should be evicted");
    assert!(
        store.get(pinned_id).await.unwrap().is_some(),
        "pinned rows must survive the byte budget"
    );
}

/// Same-instant rows must leave largest-first (the `total_byte_count DESC`
/// tie-break): when freeing the budget needs only the big row of a
/// same-`created_at` pair, evicting the small one first would then take
/// the big one too — deleting two rows where one suffices.
#[tokio::test]
async fn enforce_total_bytes_evicts_largest_first_within_one_instant() {
    let store = SqliteStore::open_memory().unwrap();
    // Insert the large row first: without the tie-break, incidental scan
    // order returns the *later* insert first within one instant, so this
    // ordering is the one where dropping `total_byte_count DESC` would
    // evict the small row before the large one and the test would catch it.
    let large_id = insert_text(&store, &"l".repeat(1000)).await;
    let small_id = insert_text(&store, &"s".repeat(10)).await;
    let same_instant = OffsetDateTime::now_utc() - time::Duration::days(1);
    backdate_entry(&store, small_id, same_instant);
    backdate_entry(&store, large_id, same_instant);

    // 1010 bytes live; a 20-byte budget is satisfied by evicting the
    // 1000-byte row alone.
    let deleted = store.enforce_total_bytes(20).await.unwrap();
    assert_eq!(deleted, 1, "evicting the large row alone frees the budget");
    assert!(
        store.get(large_id).await.unwrap().is_none(),
        "the large same-instant row should be the one evicted"
    );
    assert!(
        store.get(small_id).await.unwrap().is_some(),
        "the small same-instant row should survive"
    );
}

/// Same WAL contract as above, for the byte-budget purge path.
#[tokio::test]
async fn enforce_total_bytes_truncates_wal_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("nagori.sqlite");
    let store = SqliteStore::open(&db_path).unwrap();
    for index in 0..4 {
        insert_text(&store, &format!("byte budget wal row {index}")).await;
    }
    let wal_path = temp.path().join("nagori.sqlite-wal");
    assert!(
        wal_path.metadata().unwrap().len() > 0,
        "inserts must have written WAL frames for the truncate assertion to mean anything"
    );

    // A zero budget evicts every live, unpinned row.
    let removed = store.enforce_total_bytes(0).await.unwrap();
    assert_eq!(removed, 4);
    assert_eq!(
        wal_path.metadata().unwrap().len(),
        0,
        "byte-budget purge must checkpoint-truncate the WAL sidecar"
    );
}

#[tokio::test]
async fn enforce_total_bytes_includes_representation_payload() {
    // The retention budget must count every preserved representation
    // byte, not just the JSON envelope — otherwise a stream of large
    // images appears free and the policy never triggers eviction.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let store = SqliteStore::open_memory().unwrap();

    let big_image_bytes = {
        let mut bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10];
        bytes.resize(8 * 1024, 0xAB);
        bytes
    };
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("big-image"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/png".to_owned(),
            data: ClipboardData::Bytes(big_image_bytes.clone()),
        }],
    };
    let image_entry =
        EntryFactory::from_snapshot(snapshot).expect("png snapshot should build entry");
    let image_id = store.insert(image_entry).await.unwrap();
    let _ = insert_text(&store, "small").await;

    // 1 KiB budget is well below the image's 8 KiB body, so the image
    // row should be evicted while the text-shaped row survives.
    let deleted = store.enforce_total_bytes(1024).await.unwrap();
    assert!(deleted >= 1, "image row should be soft-deleted");
    let fetched = store.get(image_id).await.unwrap();
    assert!(
        fetched.is_none(),
        "image row should be soft-deleted by byte budget"
    );

    let entry_payload = store.get_payload(image_id).await.unwrap();
    assert!(entry_payload.is_none());
    // After eviction the live representation count drops to the
    // surviving text entry's single row.
    let live_rep_count: i64 = {
        let conn = store.conn().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM entry_representations r
             JOIN entries e ON e.id = r.entry_id
             WHERE e.deleted_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        live_rep_count, 1,
        "only the surviving text row's representation should remain live"
    );
    let _ = big_image_bytes;
}

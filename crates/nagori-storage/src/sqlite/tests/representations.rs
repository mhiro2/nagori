use nagori_core::{
    EntryFactory, EntryId, EntryRepository, RepresentationDataRef, RepresentationRole, Sensitivity,
};
use time::OffsetDateTime;

use super::super::*;

use super::insert_text;

#[tokio::test]
async fn image_payload_round_trip() {
    use nagori_core::{
        ClipboardContent, ClipboardData, ClipboardRepresentation, ClipboardSequence,
        ClipboardSnapshot,
    };

    let bytes = vec![137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4];
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("img-1"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/png".to_owned(),
            data: ClipboardData::Bytes(bytes.clone()),
        }],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield image entry");
    let id = entry.id;
    let stored = SqliteStore::open_memory().unwrap();
    let returned_id = stored.insert(entry).await.unwrap();
    assert_eq!(returned_id, id);

    let payload = stored.get_payload(id).await.unwrap();
    assert_eq!(payload, Some((bytes, "image/png".to_owned())));

    // The deserialised entry must keep its mime type and byte count, and
    // `pending_bytes` must be `None` after the round-trip — the bytes now
    // live in `entry_representations.payload_blob`, not inside `content_json`.
    let fetched = stored.get(id).await.unwrap().expect("row exists");
    match &fetched.content {
        ClipboardContent::Image(img) => {
            assert_eq!(img.byte_count, 12);
            assert_eq!(img.mime_type.as_deref(), Some("image/png"));
            assert!(img.pending_bytes.is_none());
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

#[tokio::test]
async fn get_alternate_image_payload_returns_file_list_image() {
    // A presentation copied from Finder: the file URL is the primary
    // representation, an `image/png` render rides along as an
    // alternative. The thumbnail generator reaches the image through
    // this lookup because the primary-only `get_payload` can't.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let image = vec![137u8, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3, 4]; // PNG signature
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("file-list-with-image"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(vec!["/Users/me/deck.pptx".to_owned()]),
            },
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(image.clone()),
            },
        ],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield file list");
    let id = entry.id;
    let store = SqliteStore::open_memory().unwrap();
    store.insert(entry).await.unwrap();

    // The primary is the file URL list (text), so the primary-only
    // lookup finds nothing...
    assert_eq!(store.get_payload(id).await.unwrap(), None);
    // ...but the accompanying image is reachable for the thumbnail path.
    assert_eq!(
        store.get_alternate_image_payload(id).await.unwrap(),
        Some((image, "image/png".to_owned())),
    );
}

#[tokio::test]
async fn get_alternate_image_payload_ignores_non_image_alternatives() {
    // An HTML + plain clip carries alternatives, but none are images, so
    // the image-only lookup must return None rather than a text row.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("text-no-image"),
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
        ],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
    let id = entry.id;
    let store = SqliteStore::open_memory().unwrap();
    store.insert(entry).await.unwrap();

    assert_eq!(store.get_alternate_image_payload(id).await.unwrap(), None);
}

#[tokio::test]
async fn snapshot_multi_rep_writes_one_row_per_representation() {
    // HTML + plain + RTF snapshot must produce three persisted rows so
    // a later copy-back path can re-publish whichever flavour the user
    // (or the receiving app) asks for. Without this, the multi-rep
    // promise collapses back to primary-only and pasting into a
    // markup-aware target loses the rich formatting the source offered.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("multi-rep-store"),
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
                data: ClipboardData::Text("{\\rtf1 hi}".to_owned()),
            },
        ],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
    let store = SqliteStore::open_memory().unwrap();
    let id = store.insert(entry).await.unwrap();

    let conn = store.conn().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT role, mime_type, ordinal, text_content
             FROM entry_representations
             WHERE entry_id = ?1
             ORDER BY ordinal ASC",
        )
        .unwrap();
    let rows: Vec<(String, String, i64, Option<String>)> = stmt
        .query_map([id.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, "primary");
    assert_eq!(rows[0].1, "text/html");
    assert_eq!(rows[0].2, 0);
    assert_eq!(rows[0].3.as_deref(), Some("<p>hi</p>"));

    assert_eq!(rows[1].0, "plain_fallback");
    assert_eq!(rows[1].1, "text/plain");
    assert_eq!(rows[1].2, 1);
    assert_eq!(rows[1].3.as_deref(), Some("hi"));

    assert_eq!(rows[2].0, "alternative");
    assert_eq!(rows[2].1, "application/rtf");
    assert_eq!(rows[2].2, 2);
    assert_eq!(rows[2].3.as_deref(), Some("{\\rtf1 hi}"));
}

#[tokio::test]
async fn list_representations_round_trips_role_ordinal_and_payload() {
    // Copy-back hydrates `PasteFormat::Preserve` clips through this read
    // API. Inserting an HTML+plain+RTF snapshot, then reading every row
    // back must return them in role-major (primary → plain_fallback →
    // alternative) order with payload, mime, and ordinal preserved so
    // the platform writer can republish the same multi-rep clip.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("list-rep-round-trip"),
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
                data: ClipboardData::Text("{\\rtf1 hi}".to_owned()),
            },
        ],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
    let store = SqliteStore::open_memory().unwrap();
    let id = store.insert(entry).await.unwrap();

    let reps = store.list_representations(id).await.unwrap();
    assert_eq!(reps.len(), 3);

    assert_eq!(reps[0].role, RepresentationRole::Primary);
    assert_eq!(reps[0].mime_type, "text/html");
    assert_eq!(reps[0].ordinal, 0);
    assert!(matches!(
        &reps[0].data,
        RepresentationDataRef::InlineText(text) if text == "<p>hi</p>"
    ));

    assert_eq!(reps[1].role, RepresentationRole::PlainFallback);
    assert_eq!(reps[1].mime_type, "text/plain");
    assert_eq!(reps[1].ordinal, 1);
    assert!(matches!(
        &reps[1].data,
        RepresentationDataRef::InlineText(text) if text == "hi"
    ));

    assert_eq!(reps[2].role, RepresentationRole::Alternative);
    assert_eq!(reps[2].mime_type, "application/rtf");
    assert_eq!(reps[2].ordinal, 2);
    assert!(matches!(
        &reps[2].data,
        RepresentationDataRef::InlineText(text) if text == "{\\rtf1 hi}"
    ));
}

#[tokio::test]
async fn list_representations_returns_image_blob() {
    // Image bytes are persisted in `payload_blob`; the read path must
    // surface them as `RepresentationDataRef::DatabaseBlob` so the
    // platform writer can hand the raw bytes back to NSPasteboard
    // without a UTF-8 round-trip through `text_content`.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 1, 2, 3];
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("list-rep-image"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/png".to_owned(),
            data: ClipboardData::Bytes(png_bytes.clone()),
        }],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
    let store = SqliteStore::open_memory().unwrap();
    let id = store.insert(entry).await.unwrap();

    let reps = store.list_representations(id).await.unwrap();
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].role, RepresentationRole::Primary);
    assert_eq!(reps[0].mime_type, "image/png");
    match &reps[0].data {
        RepresentationDataRef::DatabaseBlob(bytes) => assert_eq!(bytes, &png_bytes),
        other => panic!("expected DatabaseBlob, got {other:?}"),
    }
}

#[tokio::test]
async fn list_representations_decodes_file_paths_from_text_uri_list() {
    // File lists are persisted as a JSON array under the `text/uri-list`
    // mime; the read path must decode them back into a
    // `RepresentationDataRef::FilePaths` vector so the platform writer
    // can republish each path as a separate `NSPasteboardTypeFileURL`.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("list-rep-files"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/uri-list".to_owned(),
            data: ClipboardData::FilePaths(vec!["/tmp/a.txt".to_owned(), "/tmp/b.txt".to_owned()]),
        }],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
    let store = SqliteStore::open_memory().unwrap();
    let id = store.insert(entry).await.unwrap();

    let reps = store.list_representations(id).await.unwrap();
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].role, RepresentationRole::Primary);
    assert_eq!(reps[0].mime_type, "text/uri-list");
    match &reps[0].data {
        RepresentationDataRef::FilePaths(paths) => {
            assert_eq!(
                paths,
                &vec!["/tmp/a.txt".to_owned(), "/tmp/b.txt".to_owned()]
            );
        }
        other => panic!("expected FilePaths, got {other:?}"),
    }
}

#[tokio::test]
async fn list_representations_returns_empty_for_unknown_id() {
    let store = SqliteStore::open_memory().unwrap();
    let reps = store.list_representations(EntryId::new()).await.unwrap();
    assert!(reps.is_empty());
}

#[tokio::test]
async fn list_representation_summaries_batches_across_entries() {
    // Palette refreshes ask for summaries of every visible row in a
    // single round-trip. Insert two multi-rep entries plus one
    // single-rep entry, query them as a batch, and confirm each id
    // gets back its own representations in role/ordinal order with no
    // payload bytes leaking through.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let mk = |seq: &str, reps: Vec<(&str, &str)>| {
        let snapshot = ClipboardSnapshot {
            sequence: ClipboardSequence::content_hash(seq),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: reps
                .into_iter()
                .map(|(mime, payload)| ClipboardRepresentation {
                    mime_type: mime.to_owned(),
                    data: ClipboardData::Text(payload.to_owned()),
                })
                .collect(),
        };
        EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry")
    };

    let store = SqliteStore::open_memory().unwrap();
    let id_a = store
        .insert(mk(
            "batch-a",
            vec![("text/html", "<p>a</p>"), ("text/plain", "a")],
        ))
        .await
        .unwrap();
    let id_b = store
        .insert(mk("batch-b", vec![("text/plain", "b")]))
        .await
        .unwrap();
    let id_c = store
        .insert(mk(
            "batch-c",
            vec![
                ("text/html", "<i>c</i>"),
                ("text/plain", "c"),
                ("application/rtf", "{\\rtf1 c}"),
            ],
        ))
        .await
        .unwrap();

    let summaries = store
        .list_representation_summaries(&[id_a, id_b, id_c])
        .await
        .unwrap();
    assert_eq!(summaries.len(), 3);

    let a = summaries.get(&id_a).unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].role, RepresentationRole::Primary);
    assert_eq!(a[0].mime_type, "text/html");
    assert_eq!(a[0].byte_count, "<p>a</p>".len() as u64);
    assert_eq!(a[1].role, RepresentationRole::PlainFallback);
    assert_eq!(a[1].mime_type, "text/plain");

    let b = summaries.get(&id_b).unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].role, RepresentationRole::Primary);
    assert_eq!(b[0].mime_type, "text/plain");

    let c = summaries.get(&id_c).unwrap();
    assert_eq!(c.len(), 3);
    assert_eq!(c[0].role, RepresentationRole::Primary);
    assert_eq!(c[1].role, RepresentationRole::PlainFallback);
    assert_eq!(c[2].role, RepresentationRole::Alternative);
    assert_eq!(c[2].mime_type, "application/rtf");
}

#[tokio::test]
async fn list_representation_summaries_empty_input_returns_empty_map() {
    let store = SqliteStore::open_memory().unwrap();
    let summaries = store.list_representation_summaries(&[]).await.unwrap();
    assert!(summaries.is_empty());
}

#[tokio::test]
async fn list_representation_summaries_skips_soft_deleted_entries() {
    // Mirror of `list_representations_skips_soft_deleted_entries` for
    // the batch path: a soft-deleted entry must not contribute rows
    // even when its id is supplied alongside live entries.
    use nagori_core::{
        ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot,
    };

    let store = SqliteStore::open_memory().unwrap();
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("batch-soft-delete"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/plain".to_owned(),
            data: ClipboardData::Text("gone".to_owned()),
        }],
    };
    let entry = EntryFactory::from_snapshot(snapshot).expect("snapshot should yield entry");
    let id = store.insert(entry).await.unwrap();
    store.mark_deleted(id).await.unwrap();

    let summaries = store.list_representation_summaries(&[id]).await.unwrap();
    assert!(!summaries.contains_key(&id));
}

/// Build a `FileList` entry carrying `paths` at the given sensitivity,
/// using a distinct `display_text` so entries don't dedupe on insert.
fn file_list_entry(paths: &[&str], sensitivity: Sensitivity) -> nagori_core::ClipboardEntry {
    use nagori_core::{ClipboardContent, FileListContent};
    let paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
    let mut entry = EntryFactory::from_content(
        ClipboardContent::FileList(FileListContent {
            display_text: paths.join("\n"),
            paths,
        }),
        None,
        None,
    );
    entry.sensitivity = sensitivity;
    entry
}

#[tokio::test]
async fn list_file_path_sets_returns_paths_for_file_lists_only() {
    let store = SqliteStore::open_memory().unwrap();
    let files = vec!["/Users/example/Acme/a.pptx", "/Users/example/Acme/b.xlsx"];
    let file_id = store
        .insert(file_list_entry(&files, Sensitivity::Public))
        .await
        .unwrap();
    // A plain-text row is ignored even when its id rides along in the batch.
    let text_id = insert_text(&store, "not a file list").await;

    let sets = store
        .list_file_path_sets(&[file_id, text_id])
        .await
        .unwrap();
    assert_eq!(
        sets.get(&file_id),
        Some(&vec![
            "/Users/example/Acme/a.pptx".to_owned(),
            "/Users/example/Acme/b.xlsx".to_owned(),
        ])
    );
    assert!(!sets.contains_key(&text_id));
}

#[tokio::test]
async fn list_file_path_sets_only_admits_public_and_unknown() {
    // The gate must mirror `is_text_safe_for_default_output`: a sensitive
    // file list must never leak its raw paths through this batch path.
    let store = SqliteStore::open_memory().unwrap();
    let public = store
        .insert(file_list_entry(&["/pub/a.pdf"], Sensitivity::Public))
        .await
        .unwrap();
    let unknown = store
        .insert(file_list_entry(&["/unk/b.pdf"], Sensitivity::Unknown))
        .await
        .unwrap();
    let private = store
        .insert(file_list_entry(&["/priv/c.pdf"], Sensitivity::Private))
        .await
        .unwrap();
    let secret = store
        .insert(file_list_entry(&["/sec/d.pdf"], Sensitivity::Secret))
        .await
        .unwrap();
    let blocked = store
        .insert(file_list_entry(&["/blk/e.pdf"], Sensitivity::Blocked))
        .await
        .unwrap();

    let sets = store
        .list_file_path_sets(&[public, unknown, private, secret, blocked])
        .await
        .unwrap();
    assert!(sets.contains_key(&public));
    assert!(sets.contains_key(&unknown));
    assert!(!sets.contains_key(&private));
    assert!(!sets.contains_key(&secret));
    assert!(!sets.contains_key(&blocked));
}

#[tokio::test]
async fn list_file_path_sets_empty_input_returns_empty_map() {
    let store = SqliteStore::open_memory().unwrap();
    assert!(store.list_file_path_sets(&[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn list_file_path_sets_skips_soft_deleted_entries() {
    let store = SqliteStore::open_memory().unwrap();
    let id = store
        .insert(file_list_entry(&["/tmp/gone.txt"], Sensitivity::Public))
        .await
        .unwrap();
    store.mark_deleted(id).await.unwrap();
    assert!(
        !store
            .list_file_path_sets(&[id])
            .await
            .unwrap()
            .contains_key(&id)
    );
}

#[tokio::test]
async fn list_representations_skips_soft_deleted_entries() {
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_text(&store, "soft delete me").await;
    store.mark_deleted(id).await.unwrap();
    let reps = store.list_representations(id).await.unwrap();
    assert!(reps.is_empty());
}

#[tokio::test]
async fn get_payload_returns_none_for_text_entries() {
    // Text-shaped entries store their primary representation as inline
    // text, with no `payload_blob`. The preview path must therefore
    // return `None` for them so callers don't try to render the
    // representation row's `NULL` blob as image bytes.
    let store = SqliteStore::open_memory().unwrap();
    let id = insert_text(&store, "just text").await;

    let payload = store.get_payload(id).await.unwrap();
    assert!(payload.is_none());
}

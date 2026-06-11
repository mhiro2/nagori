use time::OffsetDateTime;

use super::*;
use crate::SourceApp;

#[test]
fn snapshot_text_representation_preserves_source() {
    let source = SourceApp {
        bundle_id: Some("com.example.editor".to_owned()),
        name: Some("Example Editor".to_owned()),
        executable_path: None,
    };
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("1"),
        captured_at: OffsetDateTime::now_utc(),
        source: Some(source.clone()),
        representations: vec![ClipboardRepresentation {
            mime_type: "text/plain".to_owned(),
            data: ClipboardData::Text("Clipboard text".to_owned()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

    assert_eq!(entry.plain_text(), Some("Clipboard text"));
    assert_eq!(entry.metadata.source, Some(source));
    assert_eq!(entry.search.normalized_text, "clipboard text");
}

#[test]
fn snapshot_uses_file_paths_when_text_is_absent() {
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("2"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "application/octet-stream".to_owned(),
                data: ClipboardData::Bytes(vec![1, 2, 3]),
            },
            ClipboardRepresentation {
                mime_type: "text/uri-list".to_owned(),
                data: ClipboardData::FilePaths(vec![
                    "/tmp/one.txt".to_owned(),
                    "/tmp/two.txt".to_owned(),
                ]),
            },
        ],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("file paths should build entry");

    assert_eq!(entry.plain_text(), Some("/tmp/one.txt\n/tmp/two.txt"));
}

#[test]
fn snapshot_without_recognised_data_is_ignored() {
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("3"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "application/x-unknown".to_owned(),
            data: ClipboardData::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        }],
    };

    assert!(EntryFactory::from_snapshot(snapshot).is_none());
}

#[test]
fn snapshot_image_with_mismatched_signature_falls_through_to_text() {
    // Producer labelled HTML bytes as `image/png`. The factory must
    // reject the image representation but still build an entry from
    // the sibling text/plain rep so a single misclassified payload
    // doesn't shadow legitimate clipboard content.
    let html_bytes = b"<!doctype html><html><body>oops</body></html>".to_vec();
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("img-mismatch-text"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(html_bytes),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("fallback".to_owned()),
            },
        ],
    };

    let entry =
        EntryFactory::from_snapshot(snapshot).expect("text fallback should still build an entry");
    match entry.content {
        crate::ClipboardContent::Text(text) => assert_eq!(text.text, "fallback"),
        other => panic!("expected Text fallback, got {other:?}"),
    }
}

#[test]
fn snapshot_image_only_with_invalid_signature_is_dropped() {
    // No sibling representation, so an image rep that fails the
    // magic-number check leaves the snapshot with nothing to
    // persist. The whole snapshot must be discarded rather than
    // saved as an empty / unsafe entry.
    let html_bytes = b"<!doctype html>nope".to_vec();
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("img-mismatch-only"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/png".to_owned(),
            data: ClipboardData::Bytes(html_bytes),
        }],
    };

    assert!(EntryFactory::from_snapshot(snapshot).is_none());
}

#[test]
fn snapshot_image_mime_prefix_matches_case_insensitively() {
    // IANA says the type/subtype is case-insensitive; some
    // screenshot producers publish `IMAGE/PNG`. The factory must
    // route those reps through the image branch (and therefore the
    // signature gate) instead of letting them fall through to text.
    let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("img-case"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "IMAGE/PNG".to_owned(),
            data: ClipboardData::Bytes(png_bytes),
        }],
    };

    let entry =
        EntryFactory::from_snapshot(snapshot).expect("upper-case mime should still build entry");
    assert!(matches!(entry.content, crate::ClipboardContent::Image(_)));
}

#[test]
fn snapshot_jpeg_signature_is_accepted() {
    // The factory's existing PNG test covers RFC 2083's magic; this
    // one locks down that JPEG (FF D8 FF…) also flows through the
    // signature gate so we don't regress one allow-listed format
    // while polishing another.
    let jpeg_bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("jpeg-ok"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/jpeg".to_owned(),
            data: ClipboardData::Bytes(jpeg_bytes.clone()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("jpeg should build entry");
    match entry.content {
        crate::ClipboardContent::Image(img) => {
            assert_eq!(img.mime_type.as_deref(), Some("image/jpeg"));
            assert_eq!(img.byte_count, jpeg_bytes.len());
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

#[test]
fn snapshot_tiff_signature_is_accepted() {
    // The macOS reader emits `image/tiff` for screenshots and rich-text
    // copies that carry an embedded TIFF preview. The allowlist guards
    // against bytes-vs-mime spoofing but must still accept TIFF, both
    // little-endian (II) and big-endian (MM) magic.
    let tiff_le_header = vec![0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00];
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("tiff-ok"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/tiff".to_owned(),
            data: ClipboardData::Bytes(tiff_le_header.clone()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("tiff should build entry");
    match entry.content {
        crate::ClipboardContent::Image(img) => {
            assert_eq!(img.mime_type.as_deref(), Some("image/tiff"));
            assert_eq!(img.byte_count, tiff_le_header.len());
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

#[test]
fn snapshot_bmp_is_rejected_until_paste_back_supports_it() {
    // BMP is rejected on the capture side because no platform crate can
    // publish it back to the OS clipboard. Lock the contract here so a
    // future "add BMP to the allowlist" change has to confront the
    // missing copy-back path instead of silently storing entries that
    // fail on paste.
    let bmp_header = b"BM\x46\x00\x00\x00\x00\x00\x00\x00".to_vec();
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("bmp-reject"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/bmp".to_owned(),
            data: ClipboardData::Bytes(bmp_header),
        }],
    };

    // BMP is dropped during normalisation; the only rep in the snapshot
    // was the BMP image, so `pick_primary` finds nothing and the snapshot
    // is treated as unpublishable.
    assert!(EntryFactory::from_snapshot(snapshot).is_none());
}

#[test]
fn snapshot_image_bytes_yields_image_content() {
    let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("3"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "image/png".to_owned(),
            data: ClipboardData::Bytes(png_bytes.clone()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("png should build entry");
    match &entry.content {
        crate::ClipboardContent::Image(img) => {
            assert_eq!(img.byte_count, png_bytes.len());
            assert_eq!(img.mime_type.as_deref(), Some("image/png"));
            assert_eq!(img.pending_bytes.as_deref(), Some(png_bytes.as_slice()));
        }
        other => panic!("expected Image, got {other:?}"),
    }
    // Hash must reflect the bytes, not the (empty) plain text — otherwise
    // every captured image would dedupe against the empty-string SHA.
    let expected_hash = ContentHash::sha256(&png_bytes).value;
    assert_eq!(entry.metadata.content_hash.value, expected_hash);
}

#[test]
fn snapshot_uses_captured_at_for_metadata_timestamps() {
    // The capture loop runs on a 500ms tick, so the snapshot is the
    // closest signal we have to "when did the user actually copy this".
    // `EntryMetadata` must inherit it instead of stamping `now_utc()`.
    let captured_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("4"),
        captured_at,
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/plain".to_owned(),
            data: ClipboardData::Text("hello".to_owned()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

    assert_eq!(entry.metadata.created_at, captured_at);
    assert_eq!(entry.metadata.updated_at, captured_at);
}

#[test]
fn snapshot_with_html_paired_with_text_yields_richtext_with_markup() {
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("rt-1"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>hello</p>".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("hello".to_owned()),
            },
        ],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("rich text should build entry");
    match entry.content {
        crate::ClipboardContent::RichText(rt) => {
            assert_eq!(rt.plain_text, "hello");
            assert_eq!(rt.markup.as_deref(), Some("<p>hello</p>"));
            assert_eq!(rt.markup_kind, Some(crate::RichTextMarkup::Html));
        }
        other => panic!("expected RichText, got {other:?}"),
    }
}

#[test]
fn snapshot_html_only_strips_tags_for_plain_text() {
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("rt-2"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/html".to_owned(),
            data: ClipboardData::Text("<p>hello <b>world</b></p>".to_owned()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("html-only should still build");
    match entry.content {
        crate::ClipboardContent::RichText(rt) => {
            assert_eq!(rt.plain_text, "hello world");
            assert_eq!(rt.markup_kind, Some(crate::RichTextMarkup::Html));
        }
        other => panic!("expected RichText, got {other:?}"),
    }
}

#[test]
fn strip_html_handles_angle_brackets_inside_attribute_quotes() {
    // Regression: a naive in_tag toggle would close on the `>` in href="x>y",
    // leaking attribute fragments into the plain text output.
    let stripped = super::strip_html(r#"<a href="x>y">link</a> tail"#);
    assert_eq!(stripped, "link tail");

    let stripped = super::strip_html(r#"<img alt='a > b' src="t"/>caption"#);
    assert_eq!(stripped, "caption");
}

#[test]
fn snapshot_file_paths_yields_file_list_content() {
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("fl-1"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/uri-list".to_owned(),
            data: ClipboardData::FilePaths(vec![
                "/tmp/one.txt".to_owned(),
                "/tmp/two.txt".to_owned(),
            ]),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("file list should build entry");
    match entry.content {
        crate::ClipboardContent::FileList(value) => {
            assert_eq!(value.paths.len(), 2);
            assert_eq!(value.display_text, "/tmp/one.txt\n/tmp/two.txt");
        }
        other => panic!("expected FileList, got {other:?}"),
    }
}

#[test]
fn snapshot_empty_plain_text_falls_through_to_html() {
    // Some apps (Notes, Mail) publish both a `text/plain` and a
    // `text/html` rep but leave plain empty when the source is
    // markup-only. The factory must not let the empty plain shadow the
    // non-empty html, otherwise the persisted entry has nothing to
    // search or preview.
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("rt-empty"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text(String::new()),
            },
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>hi</p>".to_owned()),
            },
        ],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("html should build entry");
    match entry.content {
        crate::ClipboardContent::RichText(rt) => {
            assert_eq!(rt.plain_text, "hi");
            assert_eq!(rt.markup.as_deref(), Some("<p>hi</p>"));
        }
        other => panic!("expected RichText, got {other:?}"),
    }
}

#[test]
fn snapshot_text_plain_with_charset_param_is_recognised() {
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("charset"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/plain;charset=utf-8".to_owned(),
            data: ClipboardData::Text("hello".to_owned()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("text should build entry");
    assert_eq!(entry.plain_text(), Some("hello"));
}

#[test]
fn from_snapshot_normalizes_with_nfkc_and_lowercase() {
    // EntryFactory must use the workspace-canonical normalizer (full-width
    // → ASCII via NFKC, then lowercase) so the search backend doesn't
    // diverge from the document it indexes.
    let captured_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("5"),
        captured_at,
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/plain".to_owned(),
            data: ClipboardData::Text("ＡＢＣ  １２３".to_owned()),
        }],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

    assert_eq!(entry.search.normalized_text, "abc 123");
}

#[test]
fn snapshot_with_html_plain_rtf_assigns_roles_in_priority_order() {
    // HTML+plain pair wins primary (RichText), the sibling plain rep
    // becomes plain_fallback (so a paste-as-plain path has it ready),
    // and the leftover RTF rep is an alternative — exactly the layering
    // the multi-rep store needs to round-trip back to the OS clipboard.
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("multi-rt"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>hello</p>".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("hello".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "application/rtf".to_owned(),
                data: ClipboardData::Text("{\\rtf1 hello}".to_owned()),
            },
        ],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
    let reps = &entry.pending_representations;
    assert_eq!(reps.len(), 3);

    assert_eq!(reps[0].role, RepresentationRole::Primary);
    assert_eq!(reps[0].mime_type, "text/html");
    assert_eq!(reps[0].ordinal, 0);

    assert_eq!(reps[1].role, RepresentationRole::PlainFallback);
    assert_eq!(reps[1].mime_type, "text/plain");
    assert_eq!(reps[1].ordinal, 1);

    assert_eq!(reps[2].role, RepresentationRole::Alternative);
    assert_eq!(reps[2].mime_type, "application/rtf");
    assert_eq!(reps[2].ordinal, 2);
}

#[test]
fn snapshot_with_image_plus_html_keeps_html_as_alternative() {
    // Image wins primary because it's the richest format the user
    // copied. The accompanying HTML rep (often a screenshot's alt-text
    // or surrounding markup) should survive as an alternative so the
    // dataset still carries the textual context — without it, full-text
    // search would lose the only searchable companion.
    let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("img-html"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(png_bytes.clone()),
            },
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>caption</p>".to_owned()),
            },
        ],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
    assert!(matches!(entry.content, crate::ClipboardContent::Image(_)));

    let reps = &entry.pending_representations;
    assert_eq!(reps.len(), 2);

    assert_eq!(reps[0].role, RepresentationRole::Primary);
    assert_eq!(reps[0].mime_type, "image/png");
    match &reps[0].data {
        RepresentationDataRef::DatabaseBlob(bytes) => assert_eq!(bytes, &png_bytes),
        other => panic!("primary should carry image bytes, got {other:?}"),
    }

    assert_eq!(reps[1].role, RepresentationRole::Alternative);
    assert_eq!(reps[1].mime_type, "text/html");
}

#[test]
fn snapshot_image_mismatch_drops_image_rep_while_text_persists() {
    // The signature-gate test above checks that the primary content
    // falls through to text; this one locks in that the bad image rep
    // doesn't leak into `pending_representations` either. A failed
    // magic-number check has to remove the rep from every downstream
    // path or storage will write bytes the daemon already rejected.
    let html_disguised_as_png = b"<!doctype html>oops".to_vec();
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("img-drop-rep"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(html_disguised_as_png),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("alt".to_owned()),
            },
        ],
    };

    let entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
    let reps = &entry.pending_representations;
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].role, RepresentationRole::Primary);
    assert_eq!(reps[0].mime_type, "text/plain");
    assert!(
        !reps.iter().any(|r| r.mime_type == "image/png"),
        "image rep with bad magic must not be persisted"
    );
}

#[test]
fn representation_set_hash_is_stable_across_equivalent_snapshots() {
    // Two snapshots whose reps differ only by `captured_at` must produce
    // the same `representation_set_hash` so dedupe can recognise "user
    // copied the same thing twice" rather than treating every paste as
    // a brand-new entry.
    let build = |captured_at: OffsetDateTime| ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("hash-stable"),
        captured_at,
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text("<p>x</p>".to_owned()),
            },
            ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("x".to_owned()),
            },
        ],
    };

    let a = EntryFactory::from_snapshot(build(
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
    ))
    .unwrap();
    let b = EntryFactory::from_snapshot(build(
        OffsetDateTime::from_unix_timestamp(1_700_000_999).unwrap(),
    ))
    .unwrap();

    let ha = a.metadata.representation_set_hash.expect("hash present");
    let hb = b.metadata.representation_set_hash.expect("hash present");
    assert_eq!(ha.value, hb.value);
}

#[test]
fn representation_set_hash_diverges_from_content_hash_when_alternatives_present() {
    // Snapshot-derived entries always emit a canonical set hash; the
    // role/mime/ordinal/sha256(payload) encoding means a multi-rep set
    // fingerprints a wider surface than the primary body alone, and the
    // two columns are expected to diverge.
    let snapshot = ClipboardSnapshot {
        sequence: crate::ClipboardSequence::content_hash("hash-diverge"),
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

    let entry = EntryFactory::from_snapshot(snapshot).unwrap();
    let set_hash = entry
        .metadata
        .representation_set_hash
        .expect("multi-rep entry must carry a representation_set_hash");
    assert_ne!(set_hash.value, entry.metadata.content_hash.value);
}

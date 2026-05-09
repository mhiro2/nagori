use time::OffsetDateTime;

use crate::{
    ClipboardContent, ClipboardData, ClipboardEntry, ClipboardRepresentation, ClipboardSnapshot,
    ContentHash, EntryId, EntryLifecycle, EntryMetadata, FileListContent, ImageContent, PayloadRef,
    RichTextContent, RichTextMarkup, SearchDocument, normalize_text,
};

#[derive(Debug, Clone)]
pub struct EntryFactory;

impl EntryFactory {
    pub fn from_text(text: impl Into<String>) -> ClipboardEntry {
        Self::from_content(ClipboardContent::from_plain_text(text.into()), None, None)
    }

    pub fn from_snapshot(snapshot: ClipboardSnapshot) -> Option<ClipboardEntry> {
        let content = pick_content(&snapshot.representations)?;
        Some(Self::from_content(
            content,
            snapshot.source,
            Some(snapshot.captured_at),
        ))
    }

    pub fn from_content(
        content: ClipboardContent,
        source: Option<crate::SourceApp>,
        captured_at: Option<OffsetDateTime>,
    ) -> ClipboardEntry {
        let id = EntryId::new();
        let plain = content.plain_text().unwrap_or_default();
        // Image entries hash their raw bytes, not the placeholder `plain`
        // string — otherwise every captured image would collide on the
        // empty-string SHA and be deduped to the first one.
        let hash = match &content {
            ClipboardContent::Image(img) => match img.pending_bytes.as_deref() {
                Some(bytes) => ContentHash::sha256(bytes),
                None => ContentHash::sha256(plain.as_bytes()),
            },
            _ => ContentHash::sha256(plain.as_bytes()),
        };
        let mut metadata = EntryMetadata::new(hash, source);
        // Prefer the snapshot's `captured_at` so the row reflects when the
        // user copied the content rather than when the daemon woke up to
        // process it. Falls back to `now_utc()` for callers that synthesise
        // entries without a snapshot (CLI `add`, tests, etc.).
        if let Some(when) = captured_at {
            metadata.created_at = when;
            metadata.updated_at = when;
        }
        let search = SearchDocument::new(id, &content, normalize_text(plain));
        ClipboardEntry {
            id,
            content,
            metadata,
            search,
            sensitivity: crate::Sensitivity::Unknown,
            lifecycle: EntryLifecycle::default(),
        }
    }
}

/// Choose the richest content representation from a clipboard snapshot.
///
/// Priority is: file URLs (`FileList`) → image bytes (`Image`) → HTML/RTF
/// paired with plain text (`RichText`) → plain text (`Text`/`Url`/`Code`
/// via `from_plain_text`) → HTML or RTF without paired plain text
/// (`RichText` with stripped/echoed body). When nothing usable is present
/// the snapshot is dropped.
fn pick_content(representations: &[ClipboardRepresentation]) -> Option<ClipboardContent> {
    if let Some(paths) = representations.iter().find_map(|rep| match &rep.data {
        ClipboardData::FilePaths(paths) if !paths.is_empty() => Some(paths.clone()),
        _ => None,
    }) {
        let display_text = paths.join("\n");
        return Some(ClipboardContent::FileList(FileListContent {
            paths,
            display_text,
        }));
    }

    if let Some((mime, bytes)) = representations.iter().find_map(|rep| match &rep.data {
        ClipboardData::Bytes(bytes) if !bytes.is_empty() && rep.mime_type.starts_with("image/") => {
            Some((rep.mime_type.clone(), bytes.clone()))
        }
        _ => None,
    }) {
        let byte_count = bytes.len();
        return Some(ClipboardContent::Image(ImageContent {
            payload_ref: PayloadRef::DatabaseBlob(String::new()),
            width: None,
            height: None,
            byte_count,
            mime_type: Some(mime),
            pending_bytes: Some(bytes),
        }));
    }

    // Skip empty bodies so an empty `text/plain` rep doesn't shadow a
    // non-empty `text/html`/`rtf` sibling. Real apps (Notes, Mail) sometimes
    // publish both with the plain side empty when the source is markup-only.
    let plain = representations
        .iter()
        .find_map(|rep| representation_text(rep, &["text/plain", "public.utf8-plain-text"]))
        .filter(|s| !s.trim().is_empty());
    let html = representations
        .iter()
        .find_map(|rep| representation_text(rep, &["text/html", "public.html"]))
        .filter(|s| !s.trim().is_empty());
    let rtf = representations
        .iter()
        .find_map(|rep| representation_text(rep, &["application/rtf", "text/rtf", "public.rtf"]))
        .filter(|s| !s.trim().is_empty());

    if let Some(plain_text) = plain {
        if let Some(markup) = html.clone() {
            return Some(ClipboardContent::RichText(RichTextContent {
                plain_text,
                payload_ref: PayloadRef::InlineText,
                markup: Some(markup),
                markup_kind: Some(RichTextMarkup::Html),
            }));
        }
        if let Some(markup) = rtf.clone() {
            return Some(ClipboardContent::RichText(RichTextContent {
                plain_text,
                payload_ref: PayloadRef::InlineText,
                markup: Some(markup),
                markup_kind: Some(RichTextMarkup::Rtf),
            }));
        }
        return Some(ClipboardContent::from_plain_text(plain_text));
    }

    if let Some(markup) = html {
        let stripped = strip_html(&markup);
        return Some(ClipboardContent::RichText(RichTextContent {
            plain_text: stripped,
            payload_ref: PayloadRef::InlineText,
            markup: Some(markup),
            markup_kind: Some(RichTextMarkup::Html),
        }));
    }
    if let Some(markup) = rtf {
        return Some(ClipboardContent::RichText(RichTextContent {
            plain_text: markup.clone(),
            payload_ref: PayloadRef::InlineText,
            markup: Some(markup),
            markup_kind: Some(RichTextMarkup::Rtf),
        }));
    }

    None
}

fn representation_text(rep: &ClipboardRepresentation, mime_types: &[&str]) -> Option<String> {
    // Match the bare mime ("text/plain") even when the rep declares a
    // suffix like "text/plain;charset=utf-8" — the parameter list doesn't
    // change which content branch we belong in.
    let bare = rep
        .mime_type
        .split(';')
        .next()
        .unwrap_or(&rep.mime_type)
        .trim();
    if !mime_types.iter().any(|m| bare.eq_ignore_ascii_case(m)) {
        return None;
    }
    match &rep.data {
        ClipboardData::Text(text) => Some(text.clone()),
        ClipboardData::Bytes(bytes) => std::str::from_utf8(bytes).ok().map(ToOwned::to_owned),
        ClipboardData::FilePaths(_) => None,
    }
}

/// Tiny tag-stripper used as a last-resort fallback when the pasteboard
/// only exposes HTML and no `text/plain`. It is intentionally lossy — full
/// HTML rendering belongs in the `WebView`, not the capture path.
///
/// Tracks attribute quoting so a `>` inside `href="x>y"` does not prematurely
/// close the tag.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut quote: Option<char> = None;
    for ch in html.chars() {
        if in_tag {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else {
                match ch {
                    '"' | '\'' => quote = Some(ch),
                    '>' => in_tag = false,
                    _ => {}
                }
            }
        } else if ch == '<' {
            in_tag = true;
            quote = None;
        } else {
            out.push(ch);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
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

        let entry =
            EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

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

        let entry =
            EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

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

        let entry =
            EntryFactory::from_snapshot(snapshot).expect("text snapshot should build entry");

        assert_eq!(entry.search.normalized_text, "abc 123");
    }
}

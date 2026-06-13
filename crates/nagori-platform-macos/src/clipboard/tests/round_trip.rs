use super::super::file_url::ns_data_to_vec;
use super::super::*;
use super::{TINY_PNG, TINY_TIFF};

use nagori_core::{
    AppError, ClipboardContent, ClipboardData, ClipboardEntry, ClipboardSnapshot, EntryFactory,
    ImageContent, RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation,
};
use nagori_platform::{ClipboardReader, ClipboardWriter};
use objc2_app_kit::{NSPasteboardTypeFileURL, NSPasteboardTypeTIFF};
use objc2_foundation::NSString;

fn image_entry(bytes: Vec<u8>, mime: &str) -> ClipboardEntry {
    let byte_count = bytes.len();
    EntryFactory::from_content(
        ClipboardContent::Image(ImageContent {
            width: Some(1),
            height: Some(1),
            byte_count,
            mime_type: Some(mime.to_owned()),
            pending_bytes: Some(bytes),
        }),
        None,
        None,
    )
}

fn snapshot_bytes(snapshot: &ClipboardSnapshot, mime: &str) -> Option<Vec<u8>> {
    snapshot
        .representations
        .iter()
        .find_map(|rep| match (&rep.data, rep.mime_type.as_str()) {
            (ClipboardData::Bytes(bytes), m) if m == mime => Some(bytes.clone()),
            _ => None,
        })
}

/// Bypass `current_snapshot` and read the raw `NSPasteboardTypeTIFF`
/// payload — the snapshot reader prefers PNG, so a stale PNG from a
/// prior step would otherwise satisfy a TIFF round-trip assertion.
fn read_pasteboard_tiff_bytes() -> Option<Vec<u8>> {
    // SAFETY: AppKit FFI on the shared pasteboard. `NSPasteboardTypeTIFF`
    // is a `'static` extern constant; the returned `Retained<NSData>`
    // owns its bytes independently of any Rust lifetime.
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let data = pb.dataForType(NSPasteboardTypeTIFF)?;
        ns_data_to_vec(&data)
    }
}

/// Read raw `NSPasteboard` data for the given UTI. JPEG / GIF / WebP
/// do not have a stable `NSPasteboardType*` constant, so a round-trip
/// assertion has to materialise an `NSString` for the UTI on the spot.
fn read_pasteboard_data_for_uti(uti: &str) -> Option<Vec<u8>> {
    let pb = NSPasteboard::generalPasteboard();
    let ty = NSString::from_str(uti);
    let data = pb.dataForType(&ty)?;
    ns_data_to_vec(&data)
}

/// Read the file URL string from the first pasteboard item. Used to
/// verify a single-file `text/uri-list` round-trip; `stringForType`
/// reports the value from the first item that carries the type, which is
/// the file item the `writeObjects` batch produced.
fn read_pasteboard_file_url_string() -> Option<String> {
    // SAFETY: AppKit FFI on the shared pasteboard. `NSPasteboardTypeFileURL`
    // is a `'static` extern constant; the returned `Retained<NSString>`
    // owns its bytes independently of any Rust lifetime.
    unsafe {
        let pb = NSPasteboard::generalPasteboard();
        let s = pb.stringForType(NSPasteboardTypeFileURL)?;
        Some(s.to_string())
    }
}

/// Cover PNG / TIFF / text / unsupported-mime / multi-rep cases in one
/// test so they share a single serialized run against the system
/// pasteboard. Splitting them into separate `#[tokio::test]`s would let
/// cargo's thread pool race them on the singleton `NSPasteboard`.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn write_entry_round_trips_image_and_text() {
    let clipboard = match MacosClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(AppError::Platform(message))
            if message.contains("selected clipboard is not supported") =>
        {
            return;
        }
        Err(err) => panic!("init MacosClipboard: {err:?}"),
    };

    let png_entry = image_entry(TINY_PNG.to_vec(), "image/png");
    clipboard
        .write_entry(&png_entry)
        .await
        .expect("write PNG entry");
    let snapshot = clipboard
        .current_snapshot()
        .await
        .expect("snapshot after PNG write");
    let png_back = snapshot_bytes(&snapshot, "image/png").expect("image/png missing from snapshot");
    assert_eq!(
        png_back, TINY_PNG,
        "PNG bytes must round-trip through NSPasteboardTypePNG verbatim"
    );

    let tiff_entry = image_entry(TINY_TIFF.to_vec(), "image/tiff");
    clipboard
        .write_entry(&tiff_entry)
        .await
        .expect("write TIFF entry");
    // Read the TIFF type directly off the pasteboard. Going through
    // `current_snapshot` here would let stale PNG bytes from the prior
    // step satisfy the assertion, since `collect_macos_extras` prefers
    // PNG when both types are present.
    let tiff_back = tokio::task::spawn_blocking(read_pasteboard_tiff_bytes)
        .await
        .expect("join blocking read");
    assert_eq!(
        tiff_back.as_deref(),
        Some(TINY_TIFF),
        "TIFF bytes must round-trip through NSPasteboardTypeTIFF verbatim"
    );

    let text_entry = EntryFactory::from_text("write_entry text fallback round-trip");
    clipboard
        .write_entry(&text_entry)
        .await
        .expect("write text entry");
    let snapshot = clipboard
        .current_snapshot()
        .await
        .expect("snapshot after text write");
    let text_back = snapshot
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
            _ => None,
        })
        .expect("text/plain missing from snapshot");
    assert_eq!(text_back, "write_entry text fallback round-trip");

    // JPEG / GIF / WebP go through dynamic UTI strings rather
    // than the static `NSPasteboardType*` constants — round-trip each
    // through its UTI to confirm the publisher's match arm fires and
    // AppKit accepts the bytes verbatim.
    for (mime, uti, bytes) in [
        (
            "image/jpeg",
            UTI_JPEG,
            &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01, 0x02][..],
        ),
        ("image/gif", UTI_GIF, &[0xCA, 0xFE, 0xBA, 0xBE][..]),
        (
            "image/webp",
            UTI_WEBP,
            &[0x52, 0x49, 0x46, 0x46, 0x77, 0x65, 0x62, 0x70][..],
        ),
    ] {
        let entry = image_entry(bytes.to_vec(), mime);
        clipboard
            .write_entry(&entry)
            .await
            .unwrap_or_else(|err| panic!("write {mime} entry: {err:?}"));
        let read_back = tokio::task::spawn_blocking(move || read_pasteboard_data_for_uti(uti))
            .await
            .expect("join blocking read");
        assert_eq!(
            read_back.as_deref(),
            Some(bytes),
            "{mime} bytes must round-trip through {uti} verbatim"
        );
    }

    let truly_unsupported = image_entry(vec![0x00, 0x01, 0x02], "image/heic");
    let err = clipboard
        .write_entry(&truly_unsupported)
        .await
        .expect_err("write_entry must reject genuinely unsupported image mime types");
    assert!(
        matches!(err, AppError::Unsupported(_)),
        "expected AppError::Unsupported for image/heic, got {err:?}"
    );

    // Preserve copy-back: a rich-text entry should land HTML + plain
    // fallback + RTF on the pasteboard in a single atomic batch so a
    // downstream paste target can pick the richest representation the
    // source originally offered.
    let host_entry =
        EntryFactory::from_text("write_representations rich-text round-trip plain body");
    let reps = vec![
        StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/html".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText(
                "<p>write_representations rich-text round-trip <strong>html</strong></p>"
                    .to_owned(),
            ),
        },
        StoredClipboardRepresentation {
            role: RepresentationRole::PlainFallback,
            mime_type: "text/plain".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::InlineText(
                "write_representations rich-text round-trip plain body".to_owned(),
            ),
        },
        StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "application/rtf".to_owned(),
            ordinal: 2,
            data: RepresentationDataRef::InlineText("{\\rtf1\\ansi rich body}".to_owned()),
        },
    ];
    clipboard
        .write_representations(&host_entry, &reps)
        .await
        .expect("write_representations multi-rep batch");
    let snapshot = clipboard
        .current_snapshot()
        .await
        .expect("snapshot after multi-rep write");
    let html_back = snapshot
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::Text(t) if rep.mime_type == "text/html" => Some(t.clone()),
            _ => None,
        })
        .expect("text/html missing from snapshot after multi-rep write");
    assert!(
        html_back.contains("<strong>html</strong>"),
        "expected HTML rep to survive the multi-rep write, got {html_back:?}"
    );
    let plain_back = snapshot
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
            _ => None,
        })
        .expect("text/plain missing from snapshot after multi-rep write");
    assert_eq!(
        plain_back, "write_representations rich-text round-trip plain body",
        "plain fallback must be published alongside the HTML primary",
    );

    // Empty representation set must fall back to write_entry semantics so
    // a caller that hands in an unhydrated list still publishes the
    // primary content rather than silently leaving the pasteboard empty.
    let fallback_entry =
        EntryFactory::from_text("write_representations empty-fallback delegates to write_entry");
    clipboard
        .write_representations(&fallback_entry, &[])
        .await
        .expect("empty representations must fall back to write_entry");
    let snapshot = clipboard
        .current_snapshot()
        .await
        .expect("snapshot after empty-fallback write");
    let fallback_back = snapshot
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
            _ => None,
        })
        .expect("text/plain missing after empty-fallback write");
    assert_eq!(
        fallback_back, "write_representations empty-fallback delegates to write_entry",
        "empty rep list should publish entry plain text via write_entry",
    );

    // Round-trip a single-file `text/uri-list` rep. The publisher writes
    // one `NSPasteboardItem` per file URL via `writeObjects`, so a
    // single-file list produces a single file item any Finder / TextEdit
    // paste target reads the same way it would a Finder copy.
    let file_url_entry = EntryFactory::from_text("/tmp/nagori-uri-list-roundtrip");
    let file_url_reps = vec![StoredClipboardRepresentation {
        role: RepresentationRole::Primary,
        mime_type: "text/uri-list".to_owned(),
        ordinal: 0,
        data: RepresentationDataRef::FilePaths(vec!["/tmp/nagori-uri-list-roundtrip".to_owned()]),
    }];
    clipboard
        .write_representations(&file_url_entry, &file_url_reps)
        .await
        .expect("write_representations file-URL rep");
    let url_back = tokio::task::spawn_blocking(read_pasteboard_file_url_string)
        .await
        .expect("join blocking read");
    assert_eq!(
        url_back.as_deref(),
        Some("file:///tmp/nagori-uri-list-roundtrip"),
        "text/uri-list rep must publish a file:// URL on NSPasteboardTypeFileURL"
    );

    // Multi-file `text/uri-list` round-trip through the multi-rep path.
    // The old per-rep `setString_forType` loop collapsed every path onto
    // the implicit item's single file-URL slot; the `NSPasteboardItem`
    // batch must instead keep every path so Finder pastes all of them.
    let multi_paths = vec![
        "/tmp/nagori-multi-one".to_owned(),
        "/tmp/nagori-multi-two".to_owned(),
        "/tmp/nagori-multi-three".to_owned(),
    ];
    let multi_entry = EntryFactory::from_text("/tmp/nagori-multi-one");
    let multi_reps = vec![StoredClipboardRepresentation {
        role: RepresentationRole::Primary,
        mime_type: "text/uri-list".to_owned(),
        ordinal: 0,
        data: RepresentationDataRef::FilePaths(multi_paths.clone()),
    }];
    clipboard
        .write_representations(&multi_entry, &multi_reps)
        .await
        .expect("write_representations multi-file uri-list");
    let multi_back = clipboard
        .current_snapshot()
        .await
        .expect("snapshot after multi-file uri-list write");
    let captured = multi_back
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::FilePaths(paths) if rep.mime_type == "text/uri-list" => {
                Some(paths.clone())
            }
            _ => None,
        })
        .expect("text/uri-list missing after multi-file write");
    assert_eq!(
        captured, multi_paths,
        "every file path must survive the multi-file copy-back, not collapse to the last URL",
    );

    // The primary-only `write_entry` FileList branch must publish files
    // too — before the fix it fell through to `plain_text()` and pasted
    // the paths as text, which never lands in Finder as files.
    let file_list_entry = EntryFactory::from_content(
        ClipboardContent::FileList(nagori_core::FileListContent {
            paths: multi_paths.clone(),
            display_text: multi_paths.join("\n"),
        }),
        None,
        None,
    );
    clipboard
        .write_entry(&file_list_entry)
        .await
        .expect("write_entry FileList branch");
    let entry_back = clipboard
        .current_snapshot()
        .await
        .expect("snapshot after write_entry FileList");
    let entry_paths = entry_back
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::FilePaths(paths) if rep.mime_type == "text/uri-list" => {
                Some(paths.clone())
            }
            _ => None,
        })
        .expect("text/uri-list missing after write_entry FileList");
    assert_eq!(
        entry_paths, multi_paths,
        "write_entry must republish a FileList as file URLs, not plain text",
    );

    // An empty FileList must be refused rather than blanking the clipboard.
    let empty_file_list = EntryFactory::from_content(
        ClipboardContent::FileList(nagori_core::FileListContent {
            paths: vec![],
            display_text: String::new(),
        }),
        None,
        None,
    );
    let empty_err = clipboard
        .write_entry(&empty_file_list)
        .await
        .expect_err("empty file-list copy-back must be refused");
    assert!(
        matches!(empty_err, AppError::Unsupported(_)),
        "expected AppError::Unsupported for an empty file list, got {empty_err:?}"
    );

    // A rep set whose MIMEs are all outside the NSPasteboard publisher's
    // table (a MIME we genuinely cannot publish — e.g. `application/pdf`)
    // must fall back to write_entry *before* `clearContents()` runs —
    // otherwise an all-unsupported set would leave the user's clipboard
    // empty and the copy command would surface a platform error.
    let only_unsupported_entry = EntryFactory::from_text(
        "write_representations falls back to write_entry when no rep is publishable",
    );
    let unsupported_reps = vec![StoredClipboardRepresentation {
        role: RepresentationRole::Primary,
        mime_type: "application/pdf".to_owned(),
        ordinal: 0,
        data: RepresentationDataRef::DatabaseBlob(vec![0x25, 0x50, 0x44, 0x46]),
    }];
    clipboard
        .write_representations(&only_unsupported_entry, &unsupported_reps)
        .await
        .expect("all-skipped rep set must fall back to write_entry, not error");
    let snapshot = clipboard
        .current_snapshot()
        .await
        .expect("snapshot after all-skipped fallback");
    let fallback_back = snapshot
        .representations
        .iter()
        .find_map(|rep| match &rep.data {
            ClipboardData::Text(t) if rep.mime_type == "text/plain" => Some(t.clone()),
            _ => None,
        })
        .expect("text/plain missing after all-skipped fallback");
    assert_eq!(
        fallback_back, "write_representations falls back to write_entry when no rep is publishable",
        "all-skipped rep set should publish entry plain text via write_entry",
    );
}

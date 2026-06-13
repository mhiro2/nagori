use super::super::file_url::{FileUrlPaths, collect_file_url_paths, oversized_file_urls};
use super::super::*;

use objc2_app_kit::{NSPasteboardItem, NSPasteboardTypeFileURL};
use objc2_foundation::{NSArray, NSString, NSURL};

fn file_url_items(urls: &[String]) -> objc2::rc::Retained<NSArray<NSPasteboardItem>> {
    let items = urls
        .iter()
        .map(|url| {
            let item = NSPasteboardItem::new();
            let value = NSString::from_str(url);
            // SAFETY: `NSPasteboardTypeFileURL` is a static AppKit
            // pasteboard type constant with framework lifetime.
            assert!(item.setString_forType(&value, unsafe { NSPasteboardTypeFileURL }));
            item
        })
        .collect::<Vec<_>>();
    NSArray::from_retained_slice(&items)
}

/// A Finder copy usually lands a *file reference URL*
/// (`file:///.file/id=…`) on the pasteboard rather than a path URL.
/// `collect_file_url_paths` must resolve it to the real filesystem path
/// instead of surfacing the literal `/.file/id=…` handle.
#[test]
fn file_reference_url_is_resolved_to_real_path() {
    // A reference URL only resolves while its target exists, so back it
    // with a real file. A unique name keeps parallel test binaries apart.
    let path = std::env::temp_dir().join(format!("nagori-file-ref-{}", std::process::id()));
    std::fs::write(&path, b"nagori").expect("write temp file");
    let real = std::fs::canonicalize(&path).expect("canonicalize temp file");

    // Mint the reference URL the way Finder does and grab its string form.
    let path_str = path.to_str().expect("temp path is valid UTF-8");
    let path_url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
    let Some(reference) = path_url.fileReferenceURL() else {
        // Some volumes (e.g. network mounts on CI) can't vend reference
        // URLs; there's nothing to assert in that environment.
        std::fs::remove_file(&path).ok();
        return;
    };
    let reference_string = reference
        .absoluteString()
        .expect("reference URL has an absolute string")
        .to_string();
    assert!(
        reference_string.contains("/.file/id="),
        "expected a file reference URL, got {reference_string}"
    );

    let items = file_url_items(&[reference_string]);
    let captured = collect_file_url_paths(&items, Some(64 * 1024));
    let FileUrlPaths::Captured(paths) = captured else {
        std::fs::remove_file(&path).ok();
        panic!("a single resolvable file reference URL must be captured");
    };
    assert_eq!(paths.len(), 1, "expected exactly one resolved path");
    let resolved = std::fs::canonicalize(&paths[0]).expect("resolved path exists");
    std::fs::remove_file(&path).ok();

    assert!(
        !paths[0].contains("/.file/id="),
        "file reference URL leaked unresolved: {}",
        paths[0]
    );
    assert_eq!(resolved, real);
}

/// A file reference URL whose target can't be resolved (here a bogus id)
/// must be dropped rather than leaking the `/.file/id=…` handle that the
/// `url` crate would otherwise decode verbatim.
#[test]
fn unresolvable_file_reference_url_is_dropped() {
    let bogus = "file:///.file/id=999999999.999999999".to_owned();
    let items = file_url_items(&[bogus]);

    let FileUrlPaths::Captured(paths) = collect_file_url_paths(&items, Some(64 * 1024)) else {
        panic!("a single file URL must be captured under the limits");
    };
    assert!(
        paths.is_empty(),
        "an unresolvable reference URL should be dropped, got {paths:?}"
    );
}

#[test]
fn file_url_paths_are_captured_under_limits() {
    let urls = vec!["file:///tmp/nagori%20one".to_owned()];
    let items = file_url_items(&urls);

    let FileUrlPaths::Captured(paths) = collect_file_url_paths(&items, Some(1024)) else {
        panic!("file URL under the byte and count limits must be captured");
    };

    assert_eq!(paths, vec!["/tmp/nagori one"]);
    assert_eq!(oversized_file_urls(&items, 1024), None);
}

#[test]
fn file_url_probe_rejects_total_utf8_bytes_before_path_allocation() {
    let urls = vec![
        "file:///tmp/nagori-alpha".to_owned(),
        "file:///tmp/nagori-beta".to_owned(),
    ];
    let items = file_url_items(&urls);
    let limit = urls[0].len();

    let Some(observed) = oversized_file_urls(&items, limit) else {
        panic!("aggregate file URL bytes above the limit must be oversized");
    };
    assert!(observed > limit);

    let FileUrlPaths::Oversized(collected_observed) = collect_file_url_paths(&items, Some(limit))
    else {
        panic!("bounded file URL collection must stop before building a full path list");
    };
    assert_eq!(collected_observed, observed);
}

#[test]
fn file_url_probe_rejects_too_many_items() {
    let urls = (0..=MAX_FILE_URL_ITEMS)
        .map(|index| format!("file:///tmp/nagori-{index}"))
        .collect::<Vec<_>>();
    let items = file_url_items(&urls);
    let limit = 1024 * 1024;

    let Some(observed) = oversized_file_urls(&items, limit) else {
        panic!("file URL count above the item limit must be oversized");
    };
    assert!(observed > limit);

    let FileUrlPaths::Oversized(collected_observed) = collect_file_url_paths(&items, Some(limit))
    else {
        panic!("bounded file URL collection must reject excessive item counts");
    };
    assert_eq!(collected_observed, observed);
}

#[test]
fn unbounded_file_url_collection_still_caps_item_count() {
    // The `current_snapshot` path passes `max_bytes = None`. Without an
    // unconditional count cap a pasteboard advertising millions of file
    // URLs would grow `paths` without bound, so the cap must fire even
    // when no byte budget is supplied.
    let urls = (0..=MAX_FILE_URL_ITEMS)
        .map(|index| format!("file:///tmp/nagori-{index}"))
        .collect::<Vec<_>>();
    let items = file_url_items(&urls);

    let FileUrlPaths::Oversized(observed) = collect_file_url_paths(&items, None) else {
        panic!("unbounded file URL collection must still reject excessive item counts");
    };
    assert!(observed > 0);

    // A list at or below the cap is still captured on the unbounded path.
    let few = (0..8)
        .map(|index| format!("file:///tmp/nagori-{index}"))
        .collect::<Vec<_>>();
    let few_items = file_url_items(&few);
    let FileUrlPaths::Captured(paths) = collect_file_url_paths(&few_items, None) else {
        panic!("a small file URL list must be captured on the unbounded path");
    };
    assert_eq!(paths.len(), few.len());
}

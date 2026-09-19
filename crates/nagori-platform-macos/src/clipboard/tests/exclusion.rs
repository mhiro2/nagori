use super::super::file_url::exclusion_for;
use super::super::write::write_pasteboard_items;
use super::super::*;

use std::time::Duration;

use nagori_platform::ClipboardExclusionKind;
use objc2::rc::Retained;
use objc2_app_kit::NSPasteboardItem;
use objc2_foundation::NSString;

/// Publish an `NSPasteboardItem` carrying `types` (each with a dummy
/// string payload) onto an *isolated* `pasteboardWithUniqueName`, so the
/// marker-detection tests never touch — or race on — the shared general
/// pasteboard that `make test` runs against.
///
/// The publish is retried because `writeObjects:` returns `NO` for a few
/// percent of calls when several test processes reach the pasteboard
/// server at once — nextest runs one process per test, so these four all
/// arrive together. A refusal there says nothing about the marker
/// detection under test, and it was observed failing a different one of
/// the four on each occurrence.
///
/// Each attempt builds a fresh pasteboard *and* a fresh item: an
/// `NSPasteboardItem` may only be written once, and a pasteboard that
/// already refused a write is not worth a second one.
fn pasteboard_with_types(types: &[&str]) -> Retained<NSPasteboard> {
    const ATTEMPTS: u32 = 5;
    for attempt in 1..=ATTEMPTS {
        let pb = NSPasteboard::pasteboardWithUniqueName();
        pb.clearContents();
        let item = NSPasteboardItem::new();
        for ty in types {
            assert!(
                item.setString_forType(&NSString::from_str("marker"), &NSString::from_str(ty)),
                "NSPasteboardItem rejected type {ty}"
            );
        }
        if write_pasteboard_items(&pb, vec![item]) {
            return pb;
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(Duration::from_millis(20 * u64::from(attempt)));
        }
    }
    panic!("NSPasteboard refused the marker item {ATTEMPTS} times (types: {types:?})");
}

#[test]
fn exclusion_for_detects_concealed_marker() {
    objc2::rc::autoreleasepool(|_| {
        let pb = pasteboard_with_types(&[MARKER_CONCEALED]);
        assert_eq!(exclusion_for(&pb), Some(ClipboardExclusionKind::Concealed));
    });
}

#[test]
fn exclusion_for_detects_transient_marker() {
    objc2::rc::autoreleasepool(|_| {
        let pb = pasteboard_with_types(&[MARKER_TRANSIENT]);
        assert_eq!(exclusion_for(&pb), Some(ClipboardExclusionKind::Transient));
    });
}

#[test]
fn exclusion_for_prefers_concealed_when_both_present() {
    objc2::rc::autoreleasepool(|_| {
        // List transient first on the item to prove the priority comes
        // from the candidate-array order in `exclusion_for`, not from the
        // order the owner happened to declare its types in.
        let pb = pasteboard_with_types(&[MARKER_TRANSIENT, MARKER_CONCEALED]);
        assert_eq!(exclusion_for(&pb), Some(ClipboardExclusionKind::Concealed));
    });
}

#[test]
fn exclusion_for_ignores_unmarked_clipboard() {
    objc2::rc::autoreleasepool(|_| {
        let pb = pasteboard_with_types(&["public.utf8-plain-text"]);
        assert_eq!(exclusion_for(&pb), None);
    });
}

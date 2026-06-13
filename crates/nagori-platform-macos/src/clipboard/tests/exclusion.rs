use super::super::*;

/// Publish an `NSPasteboardItem` carrying `types` (each with a dummy
/// string payload) onto an *isolated* `pasteboardWithUniqueName`, so the
/// marker-detection tests never touch — or race on — the shared general
/// pasteboard that `make test` runs against.
fn pasteboard_with_types(types: &[&str]) -> Retained<NSPasteboard> {
    let pb = NSPasteboard::pasteboardWithUniqueName();
    pb.clearContents();
    let item = NSPasteboardItem::new();
    for ty in types {
        assert!(
            item.setString_forType(&NSString::from_str("marker"), &NSString::from_str(ty)),
            "NSPasteboardItem rejected type {ty}"
        );
    }
    assert!(write_pasteboard_items(&pb, vec![item]));
    pb
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

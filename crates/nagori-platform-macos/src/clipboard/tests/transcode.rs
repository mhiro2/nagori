use super::super::*;
use super::TINY_TIFF;

#[test]
fn tiff_capture_is_normalized_to_png() {
    let (mime, bytes) =
        prepare_tiff_capture(TINY_TIFF.to_vec()).expect("tiny tiff passes the pixel cap");

    assert_eq!(mime, "image/png");
    assert!(bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
}

#[test]
fn prepare_tiff_capture_accepts_small_dimensions() {
    let prepared = prepare_tiff_capture(TINY_TIFF.to_vec());

    let (mime, _) = prepared.expect("tiny tiff is well under the pixel cap");
    assert_eq!(mime, "image/png");
}

#[test]
fn prepare_tiff_capture_rejects_unparseable_tiff() {
    // A TIFF whose IFD declares 65535x65535 (well over the pixel cap)
    // but whose internal strip metadata is inconsistent. The `image`
    // crate's tiff decoder refuses to surface dimensions, so
    // `prepare_tiff_capture` drops the rep instead of letting
    // `decode()` panic or allocate against a corrupt header.
    let mut tiff = TINY_TIFF.to_vec();
    tiff[18] = 0xFF; // ImageWidth low byte
    tiff[19] = 0xFF; // ImageWidth high byte
    tiff[30] = 0xFF; // ImageLength low byte
    tiff[31] = 0xFF; // ImageLength high byte

    assert!(prepare_tiff_capture(tiff).is_none());
}

#[test]
fn transcode_tiff_representations_normalizes_tiff_and_keeps_others() {
    let reps = vec![
        ClipboardRepresentation {
            mime_type: "text/plain".to_owned(),
            data: ClipboardData::Text("keep me".to_owned()),
        },
        ClipboardRepresentation {
            mime_type: "image/tiff".to_owned(),
            data: ClipboardData::Bytes(TINY_TIFF.to_vec()),
        },
    ];

    let out = transcode_tiff_representations(reps);

    assert_eq!(out.len(), 2);
    assert_eq!(out[0].mime_type, "text/plain");
    assert_eq!(out[1].mime_type, "image/png");
    match &out[1].data {
        ClipboardData::Bytes(bytes) => assert!(
            bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
            "TIFF must normalise to a PNG payload off the read timeout"
        ),
        other => panic!("expected PNG bytes, got {other:?}"),
    }
}

#[test]
fn transcode_tiff_representations_drops_undecodable_tiff() {
    // Same corrupt-dimensions TIFF as `prepare_tiff_capture_rejects_*`:
    // an undecodable image rep is dropped rather than carried forward.
    let mut tiff = TINY_TIFF.to_vec();
    tiff[18] = 0xFF;
    tiff[19] = 0xFF;
    tiff[30] = 0xFF;
    tiff[31] = 0xFF;
    let reps = vec![ClipboardRepresentation {
        mime_type: "image/tiff".to_owned(),
        data: ClipboardData::Bytes(tiff),
    }];

    assert!(transcode_tiff_representations(reps).is_empty());
}

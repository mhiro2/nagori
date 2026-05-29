import Foundation
import SwiftRs
import FoundationModels

// All exports use the C ABI (`@_cdecl`) because `swift-rs` cannot export async
// Swift functions directly (swift-rs#31). The Rust side declares matching
// `extern "C"` signatures in `bridge.rs`.

/// Sanity check that the Swift static library is linked and callable.
@_cdecl("nagori_apple_hello_c")
public func nagoriAppleHelloC() -> Int32 {
    return 42
}

/// Probe `SystemLanguageModel.default.availability` and collapse it into a
/// stable integer code consumed by `AppleAvailability::from_probe_code`:
/// 0 = available, 1 = deviceNotEligible, 2 = appleIntelligenceNotEnabled,
/// 3 = modelNotReady, 255 = unknown. `rateLimited` is intentionally absent:
/// it is a `GenerationError`, not an availability state, so it is only
/// reachable via the Rust-side mock fixture.
@_cdecl("nagori_apple_fm_availability_c")
public func nagoriAppleFmAvailabilityC() -> Int32 {
    switch SystemLanguageModel.default.availability {
    case .available:
        return 0
    case .unavailable(let reason):
        switch reason {
        case .deviceNotEligible:
            return 1
        case .appleIntelligenceNotEnabled:
            return 2
        case .modelNotReady:
            return 3
        @unknown default:
            return 255
        }
    @unknown default:
        return 255
    }
}

/// Stream cumulative UTF-8 snapshots of `sourcePtr`, one grown-by-one-character
/// snapshot at a time, mirroring the partial-snapshot shape of
/// `LanguageModelSession.streamResponse`. This exercises the Rust-side
/// longest-common-prefix delta pump and the shared cancellation path
/// end-to-end on real hardware without requiring Apple Intelligence to be
/// enabled. The real FoundationModels session is wired in Phase B.
///
/// - `isCancelled` is a Rust callback (it performs the atomic load itself, so
///   the cancel flag is never read across the language boundary); it is polled
///   before every snapshot.
/// - `onSnapshot` receives the full snapshot so far (not a delta).
/// - `onDone` receives 0 on natural completion, 1 if cancellation was observed.
@_cdecl("nagori_apple_stream_snapshots_c")
public func nagoriAppleStreamSnapshotsC(
    sourcePtr: UnsafePointer<CChar>,
    ctx: UnsafeMutableRawPointer?,
    isCancelled: @convention(c) (UnsafeMutableRawPointer?) -> UInt8,
    onSnapshot: @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>, Int) -> Void,
    onDone: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void
) {
    let text = String(cString: sourcePtr)

    // Snapshot pointer-typed args for capture in the background closure.
    let ctxCopy = ctx
    let isCancelledCopy = isCancelled
    let onSnapshotCopy = onSnapshot
    let onDoneCopy = onDone

    DispatchQueue.global(qos: .userInitiated).async {
        var snapshot = ""
        var doneCode: Int32 = 0
        for ch in text {
            if isCancelledCopy(ctxCopy) != 0 {
                doneCode = 1
                break
            }
            snapshot.append(ch)
            let bytes = Array(snapshot.utf8)
            bytes.withUnsafeBufferPointer { buf in
                if let base = buf.baseAddress {
                    onSnapshotCopy(ctxCopy, base, buf.count)
                }
            }
            // Small per-character delay so a cancel armed from Rust can be
            // observed mid-stream (the PoC measured ~30ms interception).
            usleep(2_000)
        }
        onDoneCopy(ctxCopy, doneCode)
    }
}

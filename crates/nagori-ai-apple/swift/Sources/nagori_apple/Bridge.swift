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
/// enabled. The real `FoundationModels` summarize path is
/// `nagori_apple_summarize_c` below.
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
            // observed mid-stream.
            usleep(2_000)
        }
        onDoneCopy(ctxCopy, doneCode)
    }
}

/// Summarize `promptPtr` with the on-device language model, streaming partial
/// snapshots back through `onSnapshot`. The callback contract matches
/// `nagori_apple_stream_snapshots_c`:
///
/// - `isCancelled` (a Rust callback that performs the atomic load itself) is
///   polled before forwarding each snapshot; observing cancellation stops the
///   loop and reports `onDone(ctx, 1)`.
/// - `onSnapshot` receives the cumulative partial text so far (not a delta);
///   the Rust longest-common-prefix pump turns it into ordered deltas.
/// - `onDone` reports a terminal status code: 0 success, 1 cancelled, and
///   2–8 map `LanguageModelSession.GenerationError` cases onto stable codes the
///   Rust side translates into `AiError`s.
@_cdecl("nagori_apple_summarize_c")
public func nagoriAppleSummarizeC(
    promptPtr: UnsafePointer<CChar>,
    ctx: UnsafeMutableRawPointer?,
    isCancelled: @convention(c) (UnsafeMutableRawPointer?) -> UInt8,
    onSnapshot: @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>, Int) -> Void,
    onDone: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void
) {
    let input = String(cString: promptPtr)

    let ctxCopy = ctx
    let isCancelledCopy = isCancelled
    let onSnapshotCopy = onSnapshot
    let onDoneCopy = onDone

    Task.detached(priority: .userInitiated) {
        let session = LanguageModelSession(
            instructions: """
            You are a concise summarizer. Summarize the user's text in its \
            original language. Respond with only the summary and no preamble.
            """
        )
        var doneCode: Int32 = 0
        do {
            let stream = session.streamResponse(to: input)
            for try await snapshot in stream {
                if isCancelledCopy(ctxCopy) != 0 {
                    doneCode = 1
                    break
                }
                let bytes = Array(snapshot.content.utf8)
                bytes.withUnsafeBufferPointer { buf in
                    if let base = buf.baseAddress {
                        onSnapshotCopy(ctxCopy, base, buf.count)
                    }
                }
            }
        } catch let error as LanguageModelSession.GenerationError {
            doneCode = generationErrorCode(error)
        } catch {
            doneCode = 2
        }
        onDoneCopy(ctxCopy, doneCode)
    }
}

/// Collapses a `GenerationError` into a stable status code shared with the Rust
/// `AiError` mapping. Keep in sync with `bridge::summarize_terminal`.
private func generationErrorCode(_ error: LanguageModelSession.GenerationError) -> Int32 {
    switch error {
    case .exceededContextWindowSize:
        return 3
    case .rateLimited:
        return 4
    case .assetsUnavailable:
        return 5
    case .guardrailViolation:
        return 6
    case .unsupportedLanguageOrLocale:
        return 7
    case .concurrentRequests:
        return 8
    case .decodingFailure, .unsupportedGuide, .refusal:
        return 2
    @unknown default:
        return 2
    }
}

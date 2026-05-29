import Foundation
import SwiftRs
import FoundationModels
import Translation
import NaturalLanguage

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

/// Reports whether a source/target language pair is installed, downloadable, or
/// unsupported, collapsing `LanguageAvailability.Status` into a stable code:
/// 0 = installed, 1 = supported (downloadable), 2 = unsupported, 3 = unknown
/// (no source given, or the probe did not answer within the timeout).
///
/// `LanguageAvailability.status` can hang for many seconds outside an app
/// bundle, so the async probe runs on a detached task and we bound the wait with
/// a `DispatchSemaphore`; a timeout maps to `unknown` rather than blocking the
/// caller.
@_cdecl("nagori_apple_translation_pair_status_c")
public func nagoriAppleTranslationPairStatusC(
    sourcePtr: UnsafePointer<CChar>,
    targetPtr: UnsafePointer<CChar>
) -> Int32 {
    let source = String(cString: sourcePtr)
    let target = String(cString: targetPtr)
    // Without a concrete source language there is nothing to query against
    // (`status(from:to:)` needs one), so report "unknown" and let the translate
    // path detect the source and surface a precise error if needed.
    if source.isEmpty || target.isEmpty {
        return 3
    }

    let lock = NSLock()
    var code: Int32 = 3
    let done = DispatchSemaphore(value: 0)
    Task.detached(priority: .userInitiated) {
        let availability = LanguageAvailability()
        let status = await availability.status(
            from: Locale.Language(identifier: source),
            to: Locale.Language(identifier: target)
        )
        let mapped: Int32
        switch status {
        case .installed:
            mapped = 0
        case .supported:
            mapped = 1
        case .unsupported:
            mapped = 2
        @unknown default:
            mapped = 3
        }
        lock.lock()
        code = mapped
        lock.unlock()
        done.signal()
    }
    _ = done.wait(timeout: .now() + 3.0)
    lock.lock()
    let result = code
    lock.unlock()
    return result
}

/// Guards a one-shot completion callback so it fires exactly once, no matter
/// which racing task (the translation or its timeout) reaches it first.
private final class TranslateOnce: @unchecked Sendable {
    private let lock = NSLock()
    private var fired = false
    /// Returns `true` for the first caller only.
    func claim() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if fired { return false }
        fired = true
        return true
    }
}

/// Hard cap on how long a single translation may run before the bridge gives up
/// and reports a timeout. The `Translation` framework can wedge indefinitely
/// outside an app bundle, so this bounds the FFI lifetime — `ctx` is always
/// reclaimed and the waiting Rust future always resolves — independently of any
/// consumer-side deadline. Kept just under the daemon's default request timeout
/// so the bridge usually reports first.
private let translateTimeoutNanoseconds: UInt64 = 20_000_000_000

/// Translate `textPtr` from `sourcePtr` (empty = auto-detect via
/// `NLLanguageRecognizer`) into `targetPtr`, delivering the result through a
/// single `onComplete` callback. `swift-rs` cannot export async functions
/// (swift-rs#31), so the async translation runs on a detached task and the
/// callback fires exactly once.
///
/// `onComplete(ctx, code, targetText, targetLen, detectedSource, detectedLen)`:
/// - `code` 0 means success and the two buffers carry the translated text and
///   the resolved source language code; any other code is an error (the buffers
///   are null) mapped by `bridge::translate_terminal`. `code` 6 is the timeout
///   sentinel.
/// - The byte buffers are valid only for the duration of the call; the Rust side
///   copies them out immediately.
///
/// A timeout task races the translation: whichever finishes first calls
/// `onComplete` (the loser is a no-op via [`TranslateOnce`]), so the callback
/// always fires within `translateTimeoutNanoseconds` even if the framework
/// never returns. The timeout cancels the translation task; if the framework
/// honours cancellation the native work also stops, otherwise it is abandoned
/// but `ctx` has already been reclaimed.
@_cdecl("nagori_apple_translate_c")
public func nagoriAppleTranslateC(
    textPtr: UnsafePointer<CChar>,
    sourcePtr: UnsafePointer<CChar>,
    targetPtr: UnsafePointer<CChar>,
    ctx: UnsafeMutableRawPointer?,
    onComplete: @convention(c) (
        UnsafeMutableRawPointer?, Int32, UnsafePointer<UInt8>?, Int, UnsafePointer<UInt8>?, Int
    ) -> Void
) {
    let text = String(cString: textPtr)
    let sourceArg = String(cString: sourcePtr)
    let target = String(cString: targetPtr)

    let ctxCopy = ctx
    let onCompleteCopy = onComplete
    let once = TranslateOnce()

    // Fire `onComplete` at most once. `targetText`/`detected` are non-nil only
    // on success (code 0).
    func deliver(_ code: Int32, targetText: [UInt8]?, detected: [UInt8]?) {
        guard once.claim() else { return }
        if let targetText, let detected {
            targetText.withUnsafeBufferPointer { tbuf in
                detected.withUnsafeBufferPointer { dbuf in
                    onCompleteCopy(ctxCopy, code, tbuf.baseAddress, tbuf.count, dbuf.baseAddress, dbuf.count)
                }
            }
        } else {
            onCompleteCopy(ctxCopy, code, nil, 0, nil, 0)
        }
    }

    let work = Task.detached(priority: .userInitiated) {
        // Resolve the source language: honour an explicit code, else detect the
        // dominant language of the input. Detection failure is a distinct error
        // so the caller can ask the user to pick a source language.
        let sourceCode: String
        if sourceArg.isEmpty {
            let recognizer = NLLanguageRecognizer()
            recognizer.processString(text)
            guard let dominant = recognizer.dominantLanguage else {
                deliver(3, targetText: nil, detected: nil)
                return
            }
            sourceCode = dominant.rawValue
        } else {
            sourceCode = sourceArg
        }

        let session = TranslationSession(
            installedSource: Locale.Language(identifier: sourceCode),
            target: Locale.Language(identifier: target)
        )
        // Trigger a language-pack download when one is needed and permitted; in
        // an app bundle this presents the system download sheet. A failure here
        // is swallowed so `translate` can surface the precise terminal error.
        if session.canRequestDownloads {
            try? await session.prepareTranslation()
        }

        do {
            let response = try await session.translate(text)
            let detected = response.sourceLanguage.languageCode?.identifier ?? sourceCode
            deliver(0, targetText: Array(response.targetText.utf8), detected: Array(detected.utf8))
        } catch {
            deliver(translationErrorCode(error), targetText: nil, detected: nil)
        }
    }

    Task.detached(priority: .utility) {
        try? await Task.sleep(nanoseconds: translateTimeoutNanoseconds)
        // Ask the framework to abort; if it honours cancellation the native work
        // stops, otherwise it is abandoned. Either way the callback fires now.
        work.cancel()
        deliver(6, targetText: nil, detected: nil)
    }
}

/// Collapses a `TranslationError` into a stable status code shared with the Rust
/// `AiError` mapping. Keep in sync with `bridge::translate_terminal`.
private func translationErrorCode(_ error: Error) -> Int32 {
    switch error {
    case TranslationError.notInstalled:
        return 1
    case TranslationError.unsupportedLanguagePairing,
        TranslationError.unsupportedSourceLanguage,
        TranslationError.unsupportedTargetLanguage:
        return 2
    case TranslationError.unableToIdentifyLanguage:
        return 3
    case TranslationError.nothingToTranslate:
        return 4
    default:
        return 5
    }
}

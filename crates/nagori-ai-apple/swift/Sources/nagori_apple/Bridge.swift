import Foundation
import SwiftRs
import FoundationModels
import Translation
import NaturalLanguage
import IOKit.ps

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
/// which racing task (the work or its timeout) reaches it first.
private final class OnceFlag: @unchecked Sendable {
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
    let once = OnceFlag()

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

/// Reports whether the Mac is on AC power, used by the semantic indexer's
/// battery guard: `1` = on AC (time remaining unlimited), `0` = on battery,
/// `-1` = unknown.
@_cdecl("nagori_apple_on_ac_power_c")
public func nagoriAppleOnAcPowerC() -> Int32 {
    let remaining = IOPSGetTimeRemainingEstimate()
    if remaining == kIOPSTimeRemainingUnlimited {
        return 1
    }
    if remaining == kIOPSTimeRemainingUnknown {
        return -1
    }
    return 0
}

/// Delivers the user's preferred language code (e.g. `"en"`, `"ja"`) through
/// `onResult` (invoked synchronously); falls back to `"en"`. Used to pin the
/// embedding model to the language the user's clips are most likely in. The
/// buffer is valid only for the call.
@_cdecl("nagori_apple_preferred_language_c")
public func nagoriApplePreferredLanguageC(
    ctx: UnsafeMutableRawPointer?,
    onResult: @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int) -> Void
) {
    let code =
        Locale.preferredLanguages.first
        .flatMap { Locale(identifier: $0).language.languageCode?.identifier }
        ?? Locale.current.language.languageCode?.identifier
        ?? "en"
    let bytes = Array(code.utf8)
    bytes.withUnsafeBufferPointer { buf in
        onResult(ctx, buf.baseAddress, buf.count)
    }
}

// MARK: - NLContextualEmbedding semantic index bridge

/// Process-wide cache of loaded contextual-embedding models, keyed by language
/// code, so the background indexer does not reconstruct and reload the model for
/// every clip. `NLContextualEmbedding` is reference-typed and thread-safe for
/// concurrent `embeddingResult` calls once loaded.
private final class EmbeddingCache: @unchecked Sendable {
    static let shared = EmbeddingCache()
    private let lock = NSLock()
    private var models: [String: NLContextualEmbedding] = [:]

    /// Returns a loaded embedding model for `language`, constructing and loading
    /// it on first use. `nil` if the language has no model or its assets are not
    /// installed (the caller maps that to an asset-missing error).
    func loaded(for language: String) -> NLContextualEmbedding? {
        lock.lock()
        defer { lock.unlock() }
        if let cached = models[language] {
            return cached
        }
        guard let embedding = NLContextualEmbedding(language: NLLanguage(language)),
            embedding.hasAvailableAssets
        else {
            return nil
        }
        do {
            try embedding.load()
        } catch {
            return nil
        }
        models[language] = embedding
        return embedding
    }
}

/// Hard cap on a single embedding call, mirroring the translation bridge's
/// timeout so the FFI lifetime is bounded and `ctx` is always reclaimed even if
/// the framework wedges.
private let embedTimeoutNanoseconds: UInt64 = 20_000_000_000

/// Reports whether the contextual-embedding model for `langPtr` is ready:
/// 0 = assets installed (ready), 1 = assets missing (downloadable),
/// 2 = the language has no embedding model.
@_cdecl("nagori_apple_embed_availability_c")
public func nagoriAppleEmbedAvailabilityC(langPtr: UnsafePointer<CChar>) -> Int32 {
    let language = String(cString: langPtr)
    guard let embedding = NLContextualEmbedding(language: NLLanguage(language)) else {
        return 2
    }
    return embedding.hasAvailableAssets ? 0 : 1
}

/// Reads the runtime metadata of the contextual-embedding model for `langPtr`
/// and delivers it through `onComplete` (invoked synchronously):
/// `onComplete(ctx, code, dimension, revision, maxSequenceLength, modelIdPtr,
/// modelIdLen)`. `code` 0 = success; 2 = no model for the language (numeric
/// fields 0, model id null). The model-id buffer is valid only for the call.
@_cdecl("nagori_apple_embed_metadata_c")
public func nagoriAppleEmbedMetadataC(
    langPtr: UnsafePointer<CChar>,
    ctx: UnsafeMutableRawPointer?,
    onComplete: @convention(c) (
        UnsafeMutableRawPointer?, Int32, Int, Int, Int, UnsafePointer<UInt8>?, Int
    ) -> Void
) {
    let language = String(cString: langPtr)
    guard let embedding = NLContextualEmbedding(language: NLLanguage(language)) else {
        onComplete(ctx, 2, 0, 0, 0, nil, 0)
        return
    }
    let modelId = Array(embedding.modelIdentifier.utf8)
    modelId.withUnsafeBufferPointer { buf in
        onComplete(
            ctx, 0, embedding.dimension, embedding.revision, embedding.maximumSequenceLength,
            buf.baseAddress, buf.count
        )
    }
}

/// Requests download of the contextual-embedding assets for `langPtr`, calling
/// `onComplete(ctx, code)` once: 0 = available afterwards, 1 = not available,
/// 2 = no model for the language, 5 = request errored, 6 = timed out.
@_cdecl("nagori_apple_embed_request_assets_c")
public func nagoriAppleEmbedRequestAssetsC(
    langPtr: UnsafePointer<CChar>,
    ctx: UnsafeMutableRawPointer?,
    onComplete: @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void
) {
    let language = String(cString: langPtr)
    let ctxCopy = ctx
    let onCompleteCopy = onComplete
    let once = OnceFlag()
    func deliver(_ code: Int32) {
        guard once.claim() else { return }
        onCompleteCopy(ctxCopy, code)
    }

    guard let embedding = NLContextualEmbedding(language: NLLanguage(language)) else {
        deliver(2)
        return
    }
    embedding.requestAssets { result, _ in
        switch result {
        case .available:
            deliver(0)
        case .notAvailable:
            deliver(1)
        case .error:
            deliver(5)
        @unknown default:
            deliver(5)
        }
    }

    Task.detached(priority: .utility) {
        try? await Task.sleep(nanoseconds: embedTimeoutNanoseconds)
        deliver(6)
    }
}

/// Embeds `textPtr` with the contextual-embedding model for `langPtr`,
/// mean-pooling the per-token vectors into one L2-normalised document vector and
/// delivering it through `onComplete(ctx, code, floatPtr, count)`:
/// - `code` 0 = success, `floatPtr`/`count` carry `dimension` float32 values
///   (valid only for the call; Rust copies immediately).
/// - 1 = assets missing / no model, 4 = nothing to embed, 5 = framework error,
///   6 = timed out.
///
/// A timeout task races the work so the callback always fires within
/// `embedTimeoutNanoseconds`, keeping the FFI lifetime bounded.
@_cdecl("nagori_apple_embed_c")
public func nagoriAppleEmbedC(
    langPtr: UnsafePointer<CChar>,
    textPtr: UnsafePointer<CChar>,
    ctx: UnsafeMutableRawPointer?,
    onComplete: @convention(c) (UnsafeMutableRawPointer?, Int32, UnsafePointer<Float>?, Int) -> Void
) {
    let language = String(cString: langPtr)
    let text = String(cString: textPtr)
    let ctxCopy = ctx
    let onCompleteCopy = onComplete
    let once = OnceFlag()
    func deliver(_ code: Int32, vector: [Float]?) {
        guard once.claim() else { return }
        if let vector {
            vector.withUnsafeBufferPointer { buf in
                onCompleteCopy(ctxCopy, code, buf.baseAddress, buf.count)
            }
        } else {
            onCompleteCopy(ctxCopy, code, nil, 0)
        }
    }

    let work = Task.detached(priority: .userInitiated) {
        guard let embedding = EmbeddingCache.shared.loaded(for: language) else {
            deliver(1, vector: nil)
            return
        }
        do {
            let result = try embedding.embeddingResult(for: text, language: NLLanguage(language))
            let dimension = embedding.dimension
            var sum = [Double](repeating: 0, count: dimension)
            var count = 0
            result.enumerateTokenVectors(in: text.startIndex..<text.endIndex) { vector, _ in
                if vector.count == dimension {
                    for index in 0..<dimension {
                        sum[index] += vector[index]
                    }
                    count += 1
                }
                return true
            }
            if count == 0 {
                deliver(4, vector: nil)
                return
            }
            // Mean-pool then L2-normalise so cosine distance is well-defined.
            var pooled = [Float](repeating: 0, count: dimension)
            var norm = 0.0
            for index in 0..<dimension {
                let mean = sum[index] / Double(count)
                norm += mean * mean
                pooled[index] = Float(mean)
            }
            norm = norm.squareRoot()
            if norm > 0 {
                for index in 0..<dimension {
                    pooled[index] = Float(Double(pooled[index]) / norm)
                }
            }
            deliver(0, vector: pooled)
        } catch {
            deliver(5, vector: nil)
        }
    }

    Task.detached(priority: .utility) {
        try? await Task.sleep(nanoseconds: embedTimeoutNanoseconds)
        work.cancel()
        deliver(6, vector: nil)
    }
}

use async_trait::async_trait;
#[cfg(target_os = "macos")]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use nagori_core::{AppError, PasteFailureReason, Result};
use nagori_platform::{PasteController, PasteResult};
#[cfg(target_os = "macos")]
use tracing::warn;

#[derive(Debug, Default)]
pub struct MacosPasteController;

#[async_trait]
impl PasteController for MacosPasteController {
    #[cfg(target_os = "macos")]
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        // enigo synthesises CGEvents to send Cmd+V to the focused app, which
        // requires the user to grant Accessibility permission to the running
        // process. Failures usually indicate the permission is missing.
        //
        // This is deliberately NOT bounded by a timeout the way focus-restore,
        // `frontmost_app`, and clipboard writes are. `spawn_blocking` cannot
        // cancel synthetic input: a timed-out synthesis would still post its
        // ⌘V once the wedge clears, landing the clipboard content in whatever
        // app is frontmost by then. A stray paste of (possibly sensitive)
        // history into an unrelated window is worse than the rare bounded
        // wait, and — unlike the Linux `wtype` path, which kills its
        // subprocess on timeout — the in-process `CGEvent` post has no safe
        // cancellation. Synthesis only runs on an explicit user paste, never
        // on a hot path, and a wedged WindowServer freezes the whole UI
        // anyway, so the user cannot move focus mid-wedge.
        let result = tokio::task::spawn_blocking(synthesize_cmd_v)
            .await
            .map_err(|err| AppError::Paste {
                reason: PasteFailureReason::Unknown,
                message: format!("auto-paste failed: synthesis task did not run ({err})"),
            })?;
        match result {
            Ok(()) => Ok(PasteResult {
                pasted: true,
                message: None,
            }),
            // `synthesize_cmd_v` carries the reason so an enigo
            // *initialisation* failure (an environment problem) is not
            // misreported as a missing Accessibility grant — only the
            // CGEvent *posts* are gated by that grant.
            Err((reason, message)) => Err(AppError::Paste { reason, message }),
        }
    }

    #[cfg(not(target_os = "macos"))]
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        Err(AppError::Paste {
            reason: PasteFailureReason::SynthUnsupported,
            message: "macOS auto-paste is only available on macOS".to_owned(),
        })
    }
}

/// Why a synthetic-paste step failed, paired with a human-readable message.
///
/// Splitting the reason out lets the caller tell an enigo *initialisation*
/// failure (an environment problem — no window-server session, event tap could
/// not open) from a `CGEvent` *post* failure (which the Accessibility grant
/// gates). Collapsing both into `AccessibilityMissing` previously sent users
/// to the Setup card even when the grant was present.
#[cfg(target_os = "macos")]
type SynthError = (PasteFailureReason, String);

/// Build the `AccessibilityMissing` error for a failed `CGEvent` key post.
#[cfg(target_os = "macos")]
fn key_post_error(err: &enigo::InputError) -> SynthError {
    (
        PasteFailureReason::AccessibilityMissing,
        format!("auto-paste failed (Accessibility permission may be missing): {err}"),
    )
}

#[cfg(target_os = "macos")]
fn synthesize_cmd_v() -> std::result::Result<(), SynthError> {
    // `kVK_ANSI_V` from `<HIToolbox/Events.h>` — the *physical* keycode for
    // the V key on a US ANSI keyboard. Sent via `Key::Other` so enigo skips
    // its Unicode→keycode lookup, which routes through
    // `TSMGetInputSourceProperty`; on macOS 26+ that API trips
    // `dispatch_assert_queue(main)` and aborts with SIGTRAP from any
    // non-main thread, including the tokio blocking pool we run on.
    //
    // Layout caveat: macOS resolves ⌘-shortcuts via
    // `charactersIgnoringModifiers`, and Dvorak-QWERTY⌘ swaps back to
    // QWERTY while Command is held, so this keycode triggers Paste on the
    // common QWERTY / JIS / AZERTY / ISO layouts as well as the
    // Dvorak-QWERTY⌘ variant. A user on plain Dvorak whose physical V key
    // produces a different character would not get Paste from this
    // synthesised keystroke; that case currently has to fall back to
    // manual ⌘V.
    const KVK_ANSI_V: u32 = 0x09;
    // `Enigo::new` opens the event source. A failure here is an
    // initialisation/environment problem, not the missing Accessibility grant
    // (the grant gates the CGEvent posts below), so report it as `Unknown`
    // rather than steering the user to the Accessibility Setup card.
    let mut enigo = Enigo::new(&Settings::default()).map_err(|err| {
        (
            PasteFailureReason::Unknown,
            format!("auto-paste failed: could not initialise input synthesis: {err}"),
        )
    })?;
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|err| key_post_error(&err))?;
    if let Err(err) = enigo.key(Key::Other(KVK_ANSI_V), Direction::Click) {
        // The click failed; release Meta so the user is not left with a
        // stuck modifier. A silent drop here previously meant a single
        // failed paste could leave Cmd held until the next OS-level event.
        let _ = release_meta_with_retry(&mut enigo);
        return Err(key_post_error(&err));
    }
    // The success path also has to release Meta — if this fails the user is
    // just as stuck as on the click-error path, so retry once before
    // surfacing the error.
    release_meta_with_retry(&mut enigo).map_err(|err| key_post_error(&err))
}

/// Best-effort Meta release: try once, then retry once on failure. Logs both
/// failures so a user reporting a stuck-Cmd UX bug has a breadcrumb. Returns
/// the second attempt's result so callers on the success path can surface a
/// release failure to the user instead of silently leaving ⌘ pressed.
#[cfg(target_os = "macos")]
fn release_meta_with_retry(enigo: &mut Enigo) -> std::result::Result<(), enigo::InputError> {
    let Err(first) = enigo.key(Key::Meta, Direction::Release) else {
        return Ok(());
    };
    warn!(
        target: "nagori::platform::paste",
        error = %first,
        "Meta key release failed; retrying"
    );
    enigo
        .key(Key::Meta, Direction::Release)
        .inspect_err(|second| {
            warn!(
                target: "nagori::platform::paste",
                error = %second,
                "Meta key release retry failed; ⌘ may remain virtually pressed"
            );
        })
}

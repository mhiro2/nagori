use async_trait::async_trait;
#[cfg(target_os = "macos")]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use nagori_core::{AppError, Result};
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
        let result = tokio::task::spawn_blocking(synthesize_cmd_v)
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?;
        match result {
            Ok(()) => Ok(PasteResult {
                pasted: true,
                message: None,
            }),
            Err(err) => Err(AppError::Permission(format!(
                "auto-paste failed (Accessibility permission may be missing): {err}"
            ))),
        }
    }

    #[cfg(not(target_os = "macos"))]
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        Err(AppError::Unsupported(
            "macOS auto-paste is only available on macOS".to_owned(),
        ))
    }
}

#[cfg(target_os = "macos")]
fn synthesize_cmd_v() -> std::result::Result<(), String> {
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
    let mut enigo = Enigo::new(&Settings::default()).map_err(|err| err.to_string())?;
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|err| err.to_string())?;
    if let Err(err) = enigo.key(Key::Other(KVK_ANSI_V), Direction::Click) {
        // The click failed; release Meta so the user is not left with a
        // stuck modifier. A silent drop here previously meant a single
        // failed paste could leave Cmd held until the next OS-level event.
        let _ = release_meta_with_retry(&mut enigo);
        return Err(err.to_string());
    }
    // The success path also has to release Meta — if this fails the user is
    // just as stuck as on the click-error path, so retry once before
    // surfacing the error.
    release_meta_with_retry(&mut enigo)
}

/// Best-effort Meta release: try once, then retry once on failure. Logs both
/// failures so a user reporting a stuck-Cmd UX bug has a breadcrumb. Returns
/// the second attempt's result so callers on the success path can surface a
/// release failure to the user instead of silently leaving ⌘ pressed.
#[cfg(target_os = "macos")]
fn release_meta_with_retry(enigo: &mut Enigo) -> std::result::Result<(), String> {
    let Err(first) = enigo.key(Key::Meta, Direction::Release) else {
        return Ok(());
    };
    warn!(
        target: "nagori::platform::paste",
        error = %first,
        "Meta key release failed; retrying"
    );
    enigo.key(Key::Meta, Direction::Release).map_err(|second| {
        warn!(
            target: "nagori::platform::paste",
            error = %second,
            "Meta key release retry failed; ⌘ may remain virtually pressed"
        );
        second.to_string()
    })
}

use async_trait::async_trait;
use nagori_core::{AppError, PasteFailureReason, Result};
use nagori_platform::{PasteController, PasteResult};

/// Synthesize Ctrl+V into the foreground window via `SendInput`.
///
/// Windows does not gate `SendInput` behind a TCC-style permission (UIPI is
/// the closest analogue), so the call usually just works. The notable
/// exception is when the foreground window belongs to a higher-integrity
/// process (e.g. an elevated UAC dialog): the OS silently drops the input.
/// We surface that as a generic platform error rather than guessing.
#[derive(Debug, Default)]
pub struct WindowsPasteController;

#[async_trait]
impl PasteController for WindowsPasteController {
    #[cfg(windows)]
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        // Deliberately NOT bounded by a timeout (unlike focus-restore /
        // `frontmost_app`): `spawn_blocking` cannot cancel `SendInput`, so a
        // timed-out synthesis would still inject its Ctrl+V once unwedged,
        // landing the clipboard content in whatever window is foreground by
        // then. A stray paste of (possibly sensitive) history is worse than
        // the rare bounded wait, and the in-process `SendInput` has no safe
        // cancellation the way the Linux `wtype` subprocess does. In practice
        // `SendInput` queues its events atomically and returns immediately, so
        // the wedge this would guard against is near-unreachable anyway.
        let result = tokio::task::spawn_blocking(synthesize_ctrl_v)
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
            Err(SendInputError { reason, cleanup }) => {
                // `release_ctrl_v` itself uses `SendInput`, so when UIPI or
                // an elevated foreground window blocks the press batch the
                // cleanup batch is just as likely to be dropped. In that
                // case Ctrl stays virtually held down from the OS's point
                // of view until the user taps it themselves — surface the
                // hint rather than letting the next keystroke arrive as
                // Ctrl+<whatever>.
                let message = match cleanup {
                    CleanupOutcome::Released => {
                        format!("auto-paste failed via SendInput: {reason}")
                    }
                    CleanupOutcome::Stuck { release_error } => format!(
                        "auto-paste failed via SendInput: {reason}. Ctrl key may still appear \
                         held — press Ctrl once to release it. Cleanup error: {release_error}"
                    ),
                };
                Err(AppError::Paste {
                    reason: PasteFailureReason::Unknown,
                    message,
                })
            }
        }
    }

    #[cfg(not(windows))]
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        Err(AppError::Paste {
            reason: PasteFailureReason::SynthUnsupported,
            message: "Windows auto-paste is only available on Windows".to_owned(),
        })
    }
}

/// Outcome of the press-batch attempt, threaded through to the
/// `PasteController` caller so a stuck-Ctrl scenario can surface a
/// dedicated hint instead of being collapsed into a generic `SendInput`
/// failure message.
#[cfg(windows)]
struct SendInputError {
    reason: String,
    cleanup: CleanupOutcome,
}

#[cfg(windows)]
enum CleanupOutcome {
    /// `release_ctrl_v` injected every key-up it intended — the
    /// virtual key state is consistent with "user never held Ctrl".
    Released,
    /// `release_ctrl_v` was also dropped by `SendInput`. The OS still
    /// believes Ctrl is held, so the user's next keystroke arrives as a
    /// modified one until they tap Ctrl manually.
    Stuck { release_error: String },
}

#[cfg(windows)]
fn synthesize_ctrl_v() -> std::result::Result<(), SendInputError> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        VK_CONTROL,
    };

    /// VK code for the ASCII `'V'` key. Win32 documents the virtual-key
    /// range for letters as their uppercase ASCII codepoint.
    const VK_V: u16 = b'V' as u16;

    let key_input = |vk: u16, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    // Press Ctrl, press V, release V, release Ctrl. Sent as a batch so
    // the OS routes the synthesized inputs to the same foreground window
    // even if focus shifts mid-call.
    let inputs = [
        key_input(VK_CONTROL, 0),
        key_input(VK_V, 0),
        key_input(VK_V, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];

    // The 4-element array trivially fits in `u32`, and `sizeof(INPUT)` is
    // a fixed Win32 struct size well within `i32`; `expect` panics would
    // mean a Windows SDK change, not user input.
    let input_count = u32::try_from(inputs.len()).expect("INPUT batch length fits in u32");
    let input_size =
        i32::try_from(std::mem::size_of::<INPUT>()).expect("sizeof(INPUT) fits in i32");

    // SAFETY: `inputs` is a fixed-size array of `INPUT` whose lifetime
    // outlives the call; we pass its length explicitly. `SendInput`
    // returns the number of events successfully injected.
    let sent = unsafe { SendInput(input_count, inputs.as_ptr(), input_size) };
    if sent as usize == inputs.len() {
        return Ok(());
    }

    let last_error = unsafe { GetLastError() };
    // How many of the press-batch events actually landed determines what
    // we need to undo. Ctrl-down is event 0; if `sent == 0` no key-down
    // landed and nothing needs releasing. Otherwise we must replay the
    // matching key-ups so the OS doesn't think Ctrl (and/or V) is still
    // pressed — re-using the same `SendInput` API the press used.
    let cleanup = release_ctrl_v(sent as usize, input_size, key_input);
    let reason = format!(
        "SendInput injected {sent} of {} events; GetLastError={last_error}",
        inputs.len()
    );
    Err(SendInputError { reason, cleanup })
}

#[cfg(windows)]
fn release_ctrl_v(
    sent: usize,
    input_size: i32,
    key_input: impl Fn(
        u16,
        windows_sys::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
    ) -> windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT,
) -> CleanupOutcome {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, KEYEVENTF_KEYUP, SendInput, VK_CONTROL,
    };

    const VK_V: u16 = b'V' as u16;

    // The press batch order is Ctrl-down (0), V-down (1), V-up (2),
    // Ctrl-up (3). We undo only the key-downs that actually landed and
    // were not already paired with their key-up in the same batch:
    //   sent == 1: Ctrl-down only → release Ctrl.
    //   sent == 2: Ctrl-down + V-down → release V and Ctrl.
    //   sent == 3: Ctrl-down + V-down + V-up landed → V is already up,
    //              we only need Ctrl-up. Re-sending V-up here would burn
    //              a slot on a redundant event (and could be the only
    //              slot cleanup gets, leaving Ctrl stuck), plus surface
    //              an unrequested key-up on the foreground window.
    //   sent == 4: full success, never reaches this function.
    let mut releases: Vec<INPUT> = Vec::with_capacity(2);
    if sent == 2 {
        releases.push(key_input(VK_V, KEYEVENTF_KEYUP));
    }
    if (1..=3).contains(&sent) {
        releases.push(key_input(VK_CONTROL, KEYEVENTF_KEYUP));
    }
    if releases.is_empty() {
        return CleanupOutcome::Released;
    }
    let release_count = u32::try_from(releases.len()).expect("INPUT batch length fits in u32");
    // SAFETY: `releases` is a non-empty Vec of valid key-up INPUT values
    // that lives until the call returns.
    let cleaned = unsafe { SendInput(release_count, releases.as_ptr(), input_size) };
    if cleaned as usize == releases.len() {
        CleanupOutcome::Released
    } else {
        let last_error = unsafe { GetLastError() };
        CleanupOutcome::Stuck {
            release_error: format!(
                "SendInput released {cleaned} of {} cleanup events; GetLastError={last_error}",
                releases.len()
            ),
        }
    }
}

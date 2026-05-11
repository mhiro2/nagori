use async_trait::async_trait;
use nagori_core::{AppError, Result};
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
        let result = tokio::task::spawn_blocking(synthesize_ctrl_v)
            .await
            .map_err(|err| AppError::Platform(err.to_string()))?;
        match result {
            Ok(()) => Ok(PasteResult {
                pasted: true,
                message: None,
            }),
            Err(err) => Err(AppError::Platform(format!(
                "auto-paste failed via SendInput: {err}"
            ))),
        }
    }

    #[cfg(not(windows))]
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        Err(AppError::Unsupported(
            "Windows auto-paste is only available on Windows".to_owned(),
        ))
    }
}

#[cfg(windows)]
fn synthesize_ctrl_v() -> std::result::Result<(), String> {
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
        Ok(())
    } else {
        // `GetLastError` would tell us why some events were rejected
        // (commonly `ERROR_ACCESS_DENIED` when the target is elevated),
        // but the daemon's error model only carries a `String` here.
        Err(format!(
            "SendInput injected {sent} of {} events",
            inputs.len()
        ))
    }
}

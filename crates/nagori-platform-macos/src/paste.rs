use async_trait::async_trait;
#[cfg(target_os = "macos")]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use nagori_core::{AppError, Result};
use nagori_platform::{PasteController, PasteResult};

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
    let mut enigo = Enigo::new(&Settings::default()).map_err(|err| err.to_string())?;
    enigo
        .key(Key::Meta, Direction::Press)
        .map_err(|err| err.to_string())?;
    let release_meta = |enigo: &mut Enigo| {
        let _ = enigo.key(Key::Meta, Direction::Release);
    };
    if let Err(err) = enigo.key(Key::Unicode('v'), Direction::Click) {
        release_meta(&mut enigo);
        return Err(err.to_string());
    }
    enigo
        .key(Key::Meta, Direction::Release)
        .map_err(|err| err.to_string())?;
    Ok(())
}

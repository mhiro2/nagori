use async_trait::async_trait;
use nagori_core::{AppError, Result};
use nagori_platform::{PasteController, PasteResult};

/// Synthesize Ctrl+V into the frontmost Wayland surface via `wtype`.
///
/// Wayland has no portable in-process input-synthesis API: there is no
/// equivalent of `CGEventPost` or `SendInput`. The de-facto tool is
/// `wtype`, a small CLI that talks to `zwp_virtual_keyboard_v1` on
/// compositors that expose it (Sway, KDE, Hyprland, river). Shelling
/// out keeps the daemon free of compositor-specific protocol code at
/// the cost of one process spawn per paste — acceptable because paste
/// is a user-initiated event, not a hot path.
///
/// If `wtype` is not on `$PATH` (or refuses to run because the
/// compositor doesn't expose the virtual-keyboard protocol) we surface
/// the error as `AppError::Platform` so the desktop falls back to
/// copy-only behaviour, matching the macOS / Windows "Accessibility
/// missing" semantics.
#[derive(Debug, Default)]
pub struct LinuxPasteController;

/// Upper bound on the `wtype` round-trip. A healthy compositor returns
/// in tens of milliseconds; a hung one would otherwise leave the paste
/// command pending indefinitely, blocking the runtime's paste serialisation
/// and the UI toast that surfaces the result.
#[cfg(target_os = "linux")]
const WTYPE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[async_trait]
impl PasteController for LinuxPasteController {
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        #[cfg(target_os = "linux")]
        {
            use std::process::Stdio;

            use tokio::io::AsyncReadExt;

            // Run on the blocking pool for symmetry with the macOS /
            // Windows adapters — a misbehaving compositor can keep
            // wtype waiting on a virtual-keyboard handshake for tens of
            // ms and we don't want that to pin a tokio worker.
            //
            // Spawn instead of `.output().await` so the child handle
            // survives the timeout branch and we can SIGKILL a stuck
            // `wtype` rather than leaving it hanging on the compositor.
            let mut child = tokio::process::Command::new("wtype")
                .arg("-M")
                .arg("ctrl")
                .arg("v")
                .arg("-m")
                .arg("ctrl")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|err| {
                    AppError::Platform(format!(
                        "auto-paste failed: could not invoke `wtype` ({err}). Install the \
                         `wtype` package and ensure the compositor exposes \
                         zwp_virtual_keyboard_v1.",
                    ))
                })?;
            // Drain stderr concurrently with `wait()`. If we read stderr
            // only after the child exits, a chatty `wtype` whose output
            // exceeds the pipe buffer would block on `write()` and
            // `wait()` would never return — pushing us into the timeout
            // branch *and* losing the stderr that would have explained
            // the failure. The spawned task ends naturally when the
            // child closes its stderr (on exit or `kill()`).
            let stderr_task = child.stderr.take().map(|mut pipe| {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let _ = pipe.read_to_end(&mut buf).await;
                    buf
                })
            });
            let collect_stderr = |task: Option<tokio::task::JoinHandle<Vec<u8>>>| async {
                match task {
                    Some(handle) => handle.await.unwrap_or_default(),
                    None => Vec::new(),
                }
            };
            match tokio::time::timeout(WTYPE_TIMEOUT, child.wait()).await {
                Ok(Ok(status)) if status.success() => {
                    if let Some(task) = stderr_task {
                        task.abort();
                    }
                    Ok(PasteResult {
                        pasted: true,
                        message: None,
                    })
                }
                Ok(Ok(status)) => {
                    // `wtype` writes diagnostics to stderr. Surface them so
                    // the doctor / toast layer can show the actual reason
                    // (e.g. "compositor does not support the virtual
                    // keyboard protocol") without us having to enumerate
                    // every variant.
                    let buf = collect_stderr(stderr_task).await;
                    let stderr = String::from_utf8_lossy(&buf);
                    Err(AppError::Platform(format!(
                        "auto-paste failed: wtype exited with {} ({}).",
                        status,
                        stderr.trim(),
                    )))
                }
                Ok(Err(err)) => {
                    if let Some(task) = stderr_task {
                        task.abort();
                    }
                    Err(AppError::Platform(format!(
                        "auto-paste failed: wtype wait error ({err}).",
                    )))
                }
                Err(_elapsed) => {
                    // Compositor (or wtype) is wedged. SIGKILL + reap so
                    // we don't leak a zombie, then surface the timeout
                    // as a paste failure — the caller keeps the
                    // already-completed copy and notifies the user that
                    // paste did not run.
                    if let Err(err) = child.kill().await {
                        tracing::warn!(error = %err, "wtype_kill_failed");
                    }
                    // Once the child is reaped its stderr pipe closes
                    // and the drain task completes; collecting it here
                    // gives us whatever partial diagnostic `wtype`
                    // managed to emit before getting stuck.
                    let buf = collect_stderr(stderr_task).await;
                    let stderr = String::from_utf8_lossy(&buf);
                    let stderr_tail = stderr.trim();
                    let detail = if stderr_tail.is_empty() {
                        String::new()
                    } else {
                        format!(" ({stderr_tail})")
                    };
                    Err(AppError::Platform(format!(
                        "auto-paste failed: wtype did not return within {}s. The compositor or \
                         virtual-keyboard handshake may be stuck.{detail}",
                        WTYPE_TIMEOUT.as_secs(),
                    )))
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(AppError::Unsupported(
                "Linux auto-paste is only available on Linux".to_owned(),
            ))
        }
    }
}

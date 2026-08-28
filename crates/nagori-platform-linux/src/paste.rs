use async_trait::async_trait;
use nagori_core::{AppError, PasteFailureReason, Result};
use nagori_platform::{PasteController, PasteResult};

/// Environment variable that opts a Linux Wayland session into synthetic
/// paste. Auto-paste is **off by default** on Linux — see
/// [`LinuxAutoPaste`] — and only `1` / `true` enable it.
pub const LINUX_AUTO_PASTE_ENV: &str = "NAGORI_LINUX_AUTO_PASTE";

/// Whether the Linux adapter is allowed to synthesise `Ctrl+V`.
///
/// On macOS and Windows a paste is only synthesised after the desktop shell
/// re-activated the window the user came from and the platform confirmed it
/// is frontmost again. Wayland offers neither half: the compositor exposes no
/// portable frontmost-surface query and no way for a client to re-focus
/// another surface, so `LinuxWindowBehavior` reports no restore target and
/// its restore step is a no-op that "succeeds". A synthesised `Ctrl+V` then
/// lands in whatever surface happens to hold focus after the palette hides —
/// usually the right one, but a focus handoff to another window (a
/// notification, a newly mapped surface, a compositor keybinding) would type
/// clipboard content into an unrelated app. Because that content can be a
/// secret, the safe default is copy-only, and users who accept the risk opt
/// in explicitly with [`LINUX_AUTO_PASTE_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinuxAutoPaste {
    /// Copy-only: `paste_frontmost` refuses without touching the compositor
    /// and the capability report advertises auto-paste as unsupported.
    #[default]
    Disabled,
    /// Synthesise `Ctrl+V` via `wtype` into the currently focused surface,
    /// accepting that the target cannot be verified.
    Unverified,
}

impl LinuxAutoPaste {
    /// Resolve the opt-in from the process environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var_os(LINUX_AUTO_PASTE_ENV).as_deref())
    }

    fn from_env_value(value: Option<&std::ffi::OsStr>) -> Self {
        match value.and_then(|value| value.to_str()) {
            Some(raw) if matches!(raw.trim(), "1" | "true") => Self::Unverified,
            _ => Self::Disabled,
        }
    }

    /// Why auto-paste is refused while disabled. Shared between the paste
    /// error, the capability report, and the doctor output so every surface
    /// tells the same story.
    #[must_use]
    pub fn disabled_reason() -> String {
        format!(
            "auto-paste is off by default on Linux Wayland: the compositor cannot confirm which \
             window will receive the synthesised Ctrl+V after the palette hides, so a focus \
             handoff could paste clipboard content into an unrelated app. Set \
             {LINUX_AUTO_PASTE_ENV}=1 to opt in (requires `wtype`)."
        )
    }
}

/// Synthesize Ctrl+V into the focused Wayland surface via `wtype`, when the
/// session has opted in ([`LinuxAutoPaste::Unverified`]).
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
/// the error as `AppError::Paste` so the desktop falls back to
/// copy-only behaviour, matching the macOS / Windows "Accessibility
/// missing" semantics. While auto-paste is [`LinuxAutoPaste::Disabled`]
/// (the default) the controller refuses before spawning anything.
#[derive(Debug, Default)]
pub struct LinuxPasteController {
    mode: LinuxAutoPaste,
}

impl LinuxPasteController {
    #[must_use]
    pub const fn new(mode: LinuxAutoPaste) -> Self {
        Self { mode }
    }

    /// Controller whose opt-in state follows [`LINUX_AUTO_PASTE_ENV`].
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(LinuxAutoPaste::from_env())
    }

    #[must_use]
    pub const fn mode(&self) -> LinuxAutoPaste {
        self.mode
    }
}

/// Upper bound on the `wtype` round-trip. A healthy compositor returns
/// in tens of milliseconds; a hung one would otherwise leave the paste
/// command pending indefinitely, blocking the runtime's paste serialisation
/// and the UI toast that surfaces the result.
#[cfg(target_os = "linux")]
const WTYPE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[async_trait]
impl PasteController for LinuxPasteController {
    async fn paste_frontmost(&self) -> Result<PasteResult> {
        if self.mode == LinuxAutoPaste::Disabled {
            return Err(AppError::Paste {
                reason: PasteFailureReason::SynthUnsupported,
                message: LinuxAutoPaste::disabled_reason(),
            });
        }
        synthesize_ctrl_v().await
    }
}

/// Run `wtype` to synthesise Ctrl+V, bounded by [`WTYPE_TIMEOUT`].
#[cfg(target_os = "linux")]
async fn synthesize_ctrl_v() -> Result<PasteResult> {
    use std::process::Stdio;

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
        .map_err(|err| AppError::Paste {
            reason: PasteFailureReason::ToolMissing {
                tool: "wtype".to_owned(),
            },
            message: format!(
                "auto-paste failed: could not invoke `wtype` ({err}). Install the \
                 `wtype` package and ensure the compositor exposes \
                 zwp_virtual_keyboard_v1.",
            ),
        })?;
    let stderr_task = spawn_stderr_drain(child.stderr.take());
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
            Err(AppError::Paste {
                reason: PasteFailureReason::Unknown,
                message: format!(
                    "auto-paste failed: wtype exited with {} ({}).",
                    status,
                    stderr.trim(),
                ),
            })
        }
        Ok(Err(err)) => {
            if let Some(task) = stderr_task {
                task.abort();
            }
            Err(AppError::Paste {
                reason: PasteFailureReason::Unknown,
                message: format!("auto-paste failed: wtype wait error ({err})."),
            })
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
            Err(AppError::Paste {
                reason: PasteFailureReason::Timeout,
                message: format!(
                    "auto-paste failed: wtype did not return within {}s. The compositor \
                     or virtual-keyboard handshake may be stuck.{detail}",
                    WTYPE_TIMEOUT.as_secs(),
                ),
            })
        }
    }
}

/// Drain a child's stderr concurrently with `wait()`. If we read stderr only
/// after the child exits, a chatty `wtype` whose output exceeds the pipe
/// buffer would block on `write()` and `wait()` would never return — pushing
/// us into the timeout branch *and* losing the stderr that would have
/// explained the failure. The spawned task ends naturally when the child
/// closes its stderr (on exit or `kill()`).
#[cfg(target_os = "linux")]
fn spawn_stderr_drain(
    pipe: Option<tokio::process::ChildStderr>,
) -> Option<tokio::task::JoinHandle<Vec<u8>>> {
    use tokio::io::AsyncReadExt;

    pipe.map(|mut pipe| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf).await;
            buf
        })
    })
}

#[cfg(target_os = "linux")]
async fn collect_stderr(task: Option<tokio::task::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    match task {
        Some(handle) => handle.await.unwrap_or_default(),
        None => Vec::new(),
    }
}

#[cfg(not(target_os = "linux"))]
fn synthesize_ctrl_v() -> std::future::Ready<Result<PasteResult>> {
    std::future::ready(Err(AppError::Paste {
        reason: PasteFailureReason::SynthUnsupported,
        message: "Linux auto-paste is only available on Linux".to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_paste_is_disabled_unless_the_env_opts_in() {
        assert_eq!(
            LinuxAutoPaste::from_env_value(None),
            LinuxAutoPaste::Disabled
        );
        for raw in ["0", "false", "", "yes", "on"] {
            assert_eq!(
                LinuxAutoPaste::from_env_value(Some(std::ffi::OsStr::new(raw))),
                LinuxAutoPaste::Disabled,
                "{raw:?} must not opt in"
            );
        }
        for raw in ["1", "true", " 1 "] {
            assert_eq!(
                LinuxAutoPaste::from_env_value(Some(std::ffi::OsStr::new(raw))),
                LinuxAutoPaste::Unverified,
                "{raw:?} must opt in"
            );
        }
    }

    #[tokio::test]
    async fn disabled_controller_refuses_without_synthesising() {
        // The default controller must fail closed before reaching `wtype`, and
        // classify the refusal so the desktop keeps the copy and tells the
        // user to paste manually.
        let controller = LinuxPasteController::default();
        assert_eq!(controller.mode(), LinuxAutoPaste::Disabled);
        let err = controller
            .paste_frontmost()
            .await
            .expect_err("disabled auto-paste must refuse");
        match err {
            AppError::Paste { reason, message } => {
                assert_eq!(reason, PasteFailureReason::SynthUnsupported);
                assert!(message.contains(LINUX_AUTO_PASTE_ENV), "{message}");
            }
            other => panic!("expected AppError::Paste, got {other:?}"),
        }
    }
}

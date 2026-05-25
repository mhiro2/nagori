//! macOS Quick Look adapter.
//!
//! Bridges the [`PreviewController`] trait to macOS's Quick Look by
//! spawning `qlmanage -p <files…>`. The `qlmanage` binary ships in
//! `/usr/bin/` on every supported macOS release and renders the same
//! preview overlay Finder shows when the user presses space on a
//! selected file.
//!
//! ## Why `qlmanage` rather than `QLPreviewPanel`
//!
//! `QLPreviewPanel` (`AppKit`'s in-process Quick Look surface) requires
//! the calling app to expose a delegate / data-source pair that conforms
//! to `QLPreviewPanelDataSource` and to drive `orderFront:` on the main
//! thread. That is a meaningful amount of `objc2` plumbing for a P2
//! affordance, and the user-visible result of `qlmanage -p` is
//! indistinguishable from the in-process panel — same animation, same
//! escape-to-dismiss, same Quick Look chrome. We keep the door open for
//! a future swap to `QLPreviewPanel` (e.g. once we want tighter focus
//! integration with the palette window) without changing the trait
//! signature.
//!
//! Each call spawns one `qlmanage` process — Quick Look already shares
//! its single overlay across invocations, so a second press of space
//! while a preview is up just updates the existing panel rather than
//! stacking new ones.

use std::path::PathBuf;
use std::process::{Child, Command};

use async_trait::async_trait;
use nagori_core::{AppError, Result};
use nagori_platform::{PreviewController, PreviewItem};

/// Absolute path to Apple's `qlmanage` binary. Hard-coded rather than
/// resolved via `$PATH` so a tampered shell environment cannot redirect
/// us to a different executable — this is the same defensive posture
/// the auto-paste adapter takes for `osascript`.
const QLMANAGE_PATH: &str = "/usr/bin/qlmanage";

#[derive(Debug, Default)]
pub struct MacosPreviewController;

impl MacosPreviewController {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PreviewController for MacosPreviewController {
    async fn preview(&self, items: &[PreviewItem]) -> Result<()> {
        if items.is_empty() {
            return Err(AppError::InvalidInput(
                "preview requires at least one item".to_owned(),
            ));
        }
        let paths: Vec<PathBuf> = items.iter().map(|item| item.path.clone()).collect();
        // `Command::spawn` is sync but fast — `qlmanage` forks and
        // returns immediately. Run on the blocking pool anyway so a
        // contended kernel can't stall the tokio worker.
        tokio::task::spawn_blocking(move || -> Result<()> {
            let child = spawn_qlmanage(&paths)?;
            // Reap the child on a detached OS thread so a long-lived
            // Quick Look session does not leave a zombie behind.
            // `qlmanage -p` outlives the spawn call (the panel stays up
            // until the user dismisses it), so the `wait` parks on
            // `waitpid` for as long as the user keeps the preview open.
            // Using `std::thread::spawn` rather than
            // `tokio::task::spawn_blocking` keeps that indefinite parking
            // off the tokio runtime — otherwise a runtime shutdown
            // (notably a `#[tokio::test]` returning) would block on the
            // blocking-pool worker that `waitpid` cannot be interrupted
            // out of.
            reap_child_detached(child);
            Ok(())
        })
        .await
        .map_err(|err| AppError::Platform(format!("qlmanage spawn join failed: {err}")))?
    }
}

fn spawn_qlmanage(paths: &[PathBuf]) -> Result<Child> {
    let mut command = Command::new(QLMANAGE_PATH);
    command.arg("-p");
    for path in paths {
        command.arg(path);
    }
    // Discard stdout/stderr — qlmanage chats to stderr ("Testing
    // Quick Look preview…") even on success and the bytes are
    // useless to surface in our logs. Errors come from the spawn
    // call itself.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.spawn().map_err(|err| {
        AppError::Platform(format!(
            "could not launch Quick Look (`{QLMANAGE_PATH} -p`): {err}"
        ))
    })
}

fn reap_child_detached(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_items_returns_invalid_input() {
        let controller = MacosPreviewController::new();
        match controller.preview(&[]).await {
            Err(AppError::InvalidInput(_)) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    // The happy-path call actually spawns `/usr/bin/qlmanage`. Gate it
    // on `cfg(target_os = "macos")` so a Linux/Windows CI run of the
    // workspace tests doesn't try to launch a non-existent binary —
    // the trait stub still covers cross-target coverage. We bypass
    // `MacosPreviewController::preview` and call `spawn_qlmanage`
    // directly so the test owns the `Child` handle and can kill it
    // immediately. Going through the trait would hand the child off to
    // the detached reap thread, leaving the Quick Look panel up for the
    // duration of the test binary (and, on a CI runner that never sends
    // ESC, indefinitely — that was the macos-26 CI hang).
    #[cfg(target_os = "macos")]
    #[test]
    fn spawn_with_existing_file_succeeds() {
        // Write a tiny temp file so qlmanage has something real to
        // open. We don't observe the preview window itself; the assert
        // is "spawn() didn't return an io::Error".
        let dir = std::env::temp_dir().join("nagori-preview-test");
        std::fs::create_dir_all(&dir).expect("temp dir creation");
        let path = dir.join("preview.txt");
        std::fs::write(&path, b"preview probe").expect("write probe");
        let result = spawn_qlmanage(std::slice::from_ref(&path));
        let _ = std::fs::remove_file(&path);
        let mut child = result.expect("qlmanage -p must spawn for an existing file");
        let _ = child.kill();
        let _ = child.wait();
    }
}

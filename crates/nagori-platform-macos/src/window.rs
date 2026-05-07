use async_trait::async_trait;
use nagori_core::{Result, SourceApp};
use nagori_platform::{FrontmostApp, WindowBehavior};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;

/// Thread-safety notes for the `AppKit` calls used here.
///
/// `NSWorkspace::sharedWorkspace`, `frontmostApplication`, and
/// `runningApplicationsWithBundleIdentifier` are documented as thread-safe:
/// they read snapshots of the workspace state under `AppKit`'s internal
/// locking and do not require a `CFRunLoop`. `activateWithOptions` is also
/// safe to call from a background thread — internally it posts to the main
/// thread via the `AppleEvent` dispatch.
///
/// We still hop to a blocking thread (rather than running on a tokio worker
/// directly) because each call can take a few ms when `AppKit`'s lock is
/// contended, and pinning a tokio worker for that long starves the IPC
/// handler. The blocking pool is the right place for short, sync FFI work.
#[derive(Debug, Default)]
pub struct MacosWindowBehavior;

impl MacosWindowBehavior {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Synchronous variant of `frontmost_app` so callers running in a
    /// non-async context (e.g. the Tauri global-shortcut handler) can
    /// snapshot the previous frontmost without spinning up a runtime.
    /// Callers must already be on a thread where blocking on `AppKit` is OK
    /// (typically the global-shortcut callback, which runs off the main
    /// thread but outside tokio).
    #[must_use]
    pub fn frontmost_app_blocking() -> Option<FrontmostApp> {
        frontmost_app_sync()
    }
}

#[async_trait]
impl WindowBehavior for MacosWindowBehavior {
    async fn frontmost_app(&self) -> Result<Option<FrontmostApp>> {
        // Hop off the tokio worker so a contended AppKit lock can't stall
        // IPC handlers running in parallel.
        tokio::task::spawn_blocking(frontmost_app_sync)
            .await
            .map_err(|err| nagori_core::AppError::Platform(err.to_string()))
    }

    // The Tauri shell controls the palette window directly via its own
    // commands; the daemon-side `WindowBehavior` only reports frontmost-app
    // metadata, so show / hide are no-ops here.
    async fn show_palette(&self) -> Result<()> {
        Ok(())
    }

    async fn hide_palette(&self) -> Result<()> {
        Ok(())
    }

    async fn activate_app(&self, bundle_id: &str) -> Result<()> {
        let bundle_id = bundle_id.to_owned();
        tokio::task::spawn_blocking(move || activate_app_sync(&bundle_id))
            .await
            .map_err(|err| nagori_core::AppError::Platform(err.to_string()))?;
        Ok(())
    }
}

fn frontmost_app_sync() -> Option<FrontmostApp> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app: objc2::rc::Retained<NSRunningApplication> = workspace.frontmostApplication()?;
    let bundle_id = app.bundleIdentifier().map(|s| s.to_string());
    let name = app.localizedName().map(|s| s.to_string());
    Some(FrontmostApp {
        source: SourceApp {
            bundle_id,
            name,
            executable_path: None,
        },
        window_title: None,
    })
}

fn activate_app_sync(bundle_id: &str) {
    let ns_id = NSString::from_str(bundle_id);
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&ns_id);
    let Some(app) = apps.iter().next() else {
        // Caller treats "app no longer running" as best-effort, so this
        // is not promoted to an error — the paste will land wherever the
        // OS now considers frontmost.
        return;
    };
    #[allow(deprecated)]
    let _ = app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
}

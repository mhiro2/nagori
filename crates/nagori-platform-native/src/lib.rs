//! Per-OS adapter wiring for the production runtime.
//!
//! Both `nagori-cli` (daemon mode + direct copy/paste commands) and the
//! desktop app construct a `NagoriRuntime` plus the auxiliary handles a
//! capture loop / window-behavior consumer needs. Before this crate
//! existed the two call sites each carried three `cfg(target_os)` arms
//! that picked clipboard / paste / permissions / window adapters from
//! the per-OS platform crates, which let small differences (Linux
//! Wayland error annotation in desktop only, missing `permissions` on
//! one branch, etc.) creep in. The shared `build_native_runtime`
//! collapses that into one definition.

use std::path::PathBuf;
use std::sync::Arc;

use nagori_ai::{AiProvider, LocalAiProvider};
#[cfg(any(
    target_os = "linux",
    not(any(target_os = "macos", target_os = "windows", target_os = "linux"))
))]
use nagori_core::AppError;
use nagori_core::Result;
use nagori_daemon::NagoriRuntime;
use nagori_platform::{ClipboardReader, PlatformCapabilities, WindowBehavior};
use nagori_storage::SqliteStore;

/// Outputs of [`build_native_runtime`]: the runtime itself plus the
/// adapter handles that callers commonly need to expose to other
/// subsystems (capture loop, palette refocus).
pub struct NativeRuntimeParts {
    pub runtime: NagoriRuntime,
    /// Same underlying clipboard object that the runtime writes to —
    /// holding the reader Arc separately lets the capture loop share
    /// state with the writer instead of polling a different adapter.
    pub clipboard_reader: Arc<dyn ClipboardReader>,
    pub window: Arc<dyn WindowBehavior>,
}

/// Optional overrides for [`build_native_runtime`]. Defaults match the
/// production call sites: `LocalAiProvider`, no preset socket path.
#[derive(Default)]
pub struct NativeRuntimeOptions {
    /// Socket path threaded into the runtime so the IPC `Doctor` /
    /// `Health` reports can echo it back. Daemon callers pass the
    /// resolved endpoint; library callers (desktop) leave it unset.
    pub socket_path: Option<PathBuf>,
    /// Override the AI provider. When `None`, `LocalAiProvider::default()`
    /// is wired — matches both CLI and desktop production behaviour.
    pub ai: Option<Arc<dyn AiProvider>>,
}

/// Build a production runtime backed by the host OS's adapters.
///
/// Wires clipboard / paste / permission / window from the per-OS
/// platform crate, and returns the runtime alongside a clipboard
/// reader handle (for the capture loop) and the window-behavior
/// adapter.
///
/// On unsupported targets the function returns `AppError::Unsupported`
/// — daemon and desktop both refuse to start there, so producing a
/// dummy runtime that would silently never capture or paste would be a
/// footgun.
pub fn build_native_runtime(
    store: SqliteStore,
    options: NativeRuntimeOptions,
) -> Result<NativeRuntimeParts> {
    build_native_runtime_inner(store, options)
}

/// Report the host adapter's capability matrix.
///
/// Dispatches to the per-OS `report_capabilities` using the same
/// `cfg(target_os)` arms as [`build_native_runtime`] so the capability
/// view and the wired adapters can never disagree about which OS the
/// runtime is running on. Unsupported targets fall back to
/// [`nagori_platform::unsupported_capabilities`].
#[must_use]
pub fn capabilities() -> PlatformCapabilities {
    capabilities_inner()
}

#[cfg(target_os = "macos")]
fn capabilities_inner() -> PlatformCapabilities {
    nagori_platform_macos::report_capabilities()
}

#[cfg(target_os = "windows")]
fn capabilities_inner() -> PlatformCapabilities {
    nagori_platform_windows::report_capabilities()
}

#[cfg(target_os = "linux")]
fn capabilities_inner() -> PlatformCapabilities {
    nagori_platform_linux::report_capabilities()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn capabilities_inner() -> PlatformCapabilities {
    nagori_platform::unsupported_capabilities()
}

#[cfg(target_os = "macos")]
fn build_native_runtime_inner(
    store: SqliteStore,
    options: NativeRuntimeOptions,
) -> Result<NativeRuntimeParts> {
    use nagori_platform_macos::{
        MacosClipboard, MacosPasteController, MacosPermissionChecker, MacosWindowBehavior,
    };

    let clipboard = Arc::new(MacosClipboard::new()?);
    let clipboard_reader: Arc<dyn ClipboardReader> = clipboard.clone();
    let window: Arc<dyn WindowBehavior> = Arc::new(MacosWindowBehavior::new());
    let runtime = assemble_runtime(
        store,
        clipboard,
        Arc::new(MacosPasteController),
        Arc::new(MacosPermissionChecker),
        options,
    )?;
    Ok(NativeRuntimeParts {
        runtime,
        clipboard_reader,
        window,
    })
}

#[cfg(target_os = "windows")]
fn build_native_runtime_inner(
    store: SqliteStore,
    options: NativeRuntimeOptions,
) -> Result<NativeRuntimeParts> {
    use nagori_platform_windows::{
        WindowsClipboard, WindowsPasteController, WindowsPermissionChecker, WindowsWindowBehavior,
    };

    let clipboard = Arc::new(WindowsClipboard::new()?);
    let clipboard_reader: Arc<dyn ClipboardReader> = clipboard.clone();
    let window: Arc<dyn WindowBehavior> = Arc::new(WindowsWindowBehavior::new());
    let runtime = assemble_runtime(
        store,
        clipboard,
        Arc::new(WindowsPasteController),
        Arc::new(WindowsPermissionChecker),
        options,
    )?;
    Ok(NativeRuntimeParts {
        runtime,
        clipboard_reader,
        window,
    })
}

#[cfg(target_os = "linux")]
fn build_native_runtime_inner(
    store: SqliteStore,
    options: NativeRuntimeOptions,
) -> Result<NativeRuntimeParts> {
    use nagori_platform_linux::{
        LinuxClipboard, LinuxPasteController, LinuxPermissionChecker, LinuxWindowBehavior,
    };

    // Annotate the platform error with Wayland-specific guidance: the
    // typical cause is a compositor without `wl_data_control` or an X11
    // session. Without this wrapper users see a bare
    // `AppError::Platform(…)` and can't tell whether it's transient or
    // an architectural constraint of their desktop environment.
    let clipboard = Arc::new(LinuxClipboard::new().map_err(annotate_linux_clipboard_error)?);
    let clipboard_reader: Arc<dyn ClipboardReader> = clipboard.clone();
    let window: Arc<dyn WindowBehavior> = Arc::new(LinuxWindowBehavior::new());
    let runtime = assemble_runtime(
        store,
        clipboard,
        Arc::new(LinuxPasteController),
        Arc::new(LinuxPermissionChecker),
        options,
    )?;
    Ok(NativeRuntimeParts {
        runtime,
        clipboard_reader,
        window,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn build_native_runtime_inner(
    _store: SqliteStore,
    _options: NativeRuntimeOptions,
) -> Result<NativeRuntimeParts> {
    Err(AppError::Unsupported(
        "Nagori native runtime is supported on macOS, Windows, and Linux only".to_owned(),
    ))
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
fn assemble_runtime<C, P, K>(
    store: SqliteStore,
    clipboard: Arc<C>,
    paste: Arc<P>,
    permissions: Arc<K>,
    options: NativeRuntimeOptions,
) -> Result<NagoriRuntime>
where
    C: nagori_platform::ClipboardWriter + 'static,
    P: nagori_platform::PasteController + 'static,
    K: nagori_platform::PermissionChecker + 'static,
{
    let ai: Arc<dyn AiProvider> = options
        .ai
        .unwrap_or_else(|| Arc::new(LocalAiProvider::default()));
    let mut builder = NagoriRuntime::builder(store)
        .clipboard(clipboard)
        .paste(paste)
        .ai(ai)
        .permissions(permissions)
        .capabilities(capabilities());
    if let Some(socket_path) = options.socket_path {
        builder = builder.socket_path(socket_path);
    }
    builder.build()
}

#[cfg(target_os = "linux")]
fn annotate_linux_clipboard_error(err: AppError) -> AppError {
    // Preserve the original variant so the CLI's exit-code mapping stays
    // stable across the refactor — `LinuxClipboard::new()` returns
    // `Unsupported` for "compositor lacks wl_data_control / X11 session"
    // and `Platform` for other failures, and those exit as 7 and 8
    // respectively. Without this split everything would funnel into 8.
    const HINT: &str = "Nagori requires a Wayland session whose compositor supports the \
         `wl_data_control` protocol (wlroots-based compositors such as \
         sway/Hyprland qualify; GNOME Wayland currently does not). \
         X11 is not supported.";
    match err {
        AppError::Unsupported(message) => AppError::Unsupported(format!(
            "could not initialise the Linux clipboard adapter: {message}. {HINT}"
        )),
        other => AppError::Platform(format!(
            "could not initialise the Linux clipboard adapter: {other}. {HINT}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nagori_platform::{Platform, SupportTier};

    #[test]
    fn capabilities_match_host_target() {
        let caps = capabilities();
        // The cfg arms below must stay in lockstep with the
        // `capabilities_inner` arms above — if a new supported target
        // is added there, it must show up here too.
        #[cfg(target_os = "macos")]
        {
            assert_eq!(caps.platform, Platform::MacOS);
            assert_eq!(caps.tier, SupportTier::Supported);
        }
        #[cfg(target_os = "windows")]
        {
            assert_eq!(caps.platform, Platform::Windows);
            assert_eq!(caps.tier, SupportTier::Experimental);
        }
        #[cfg(target_os = "linux")]
        {
            assert_eq!(caps.platform, Platform::LinuxWayland);
            assert_eq!(caps.tier, SupportTier::Experimental);
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            assert_eq!(caps.platform, Platform::Unsupported);
            assert_eq!(caps.tier, SupportTier::Unsupported);
        }
    }

    #[test]
    fn options_default_is_empty() {
        let options = NativeRuntimeOptions::default();
        assert!(options.socket_path.is_none());
        assert!(options.ai.is_none());
    }

    // Smoke-test the host target's wiring: the helper must produce a
    // runtime with adapters wired (so `NagoriRuntimeBuilder::build`
    // does not return `AppError::Configuration`). Skipped on Linux
    // because `LinuxClipboard::new` opens a live Wayland connection,
    // which CI runners (`ubuntu-latest`) don't provide. macOS and
    // Windows runners initialise their native adapters synchronously
    // without a similar dependency.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn build_native_runtime_succeeds_on_host_target() {
        let store = SqliteStore::open_memory().expect("memory store");
        let parts = build_native_runtime(store, NativeRuntimeOptions::default())
            .expect("native runtime wires the host adapters");
        // The reader / window are non-null trait objects by construction;
        // accessing them here also asserts the public fields stay public.
        let _ = parts.runtime.store();
        let _: Arc<dyn ClipboardReader> = parts.clipboard_reader;
        let _: Arc<dyn WindowBehavior> = parts.window;
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    #[test]
    fn build_native_runtime_errors_on_unsupported_target() {
        let store = SqliteStore::open_memory().expect("memory store");
        match build_native_runtime(store, NativeRuntimeOptions::default()) {
            Err(AppError::Unsupported(_)) => {}
            Err(err) => panic!("expected Unsupported, got {err:?}"),
            Ok(_) => panic!("expected Unsupported on this host target"),
        }
    }
}

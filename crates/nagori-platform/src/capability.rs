//! Capability model describing what each host OS adapter actually supports.
//!
//! Per-OS differences leak through clipboard write errors, permission
//! probes, hotkey-registration errors, and `AppError::Unsupported`
//! returns — all in different shapes. Without a single source of truth,
//! UI callers cannot reliably distinguish "you need to grant a
//! permission", "your OS does not support this", "install `wtype` to
//! enable this", and "this is experimental and may misbehave".
//! `PlatformCapabilities` is that single source.
//!
//! The model is intentionally OS-static (driven by `cfg(target_os)`)
//! rather than probing live state. Dynamic checks — whether the
//! Accessibility permission is currently granted, whether `wtype` is
//! on `$PATH` right now — already have dedicated probes
//! (`PermissionChecker`, the auto-paste path). Capabilities answer the
//! coarser question "could this feature work on this OS at all", and
//! the UI layers the two together.

use serde::{Deserialize, Serialize};

use crate::permissions::PermissionKind;

/// High-level family the runtime is currently targeting. Matches the
/// `cfg(target_os)` arms in [`super::permissions`] and the platform
/// crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    // The default snake_case derive would rename `MacOS` to `mac_o_s`.
    // Override explicitly so the IPC / CLI JSON shape stays the natural
    // `"macos"`, which is the contract we want pinned now that
    // `PlatformCapabilities` is a public surface.
    #[serde(rename = "macos")]
    MacOS,
    Windows,
    LinuxWayland,
    Unsupported,
}

/// Overall maturity of the platform port. Mirrors the README support
/// table: macOS is first-class, Windows and Linux Wayland are
/// experimental, everything else is unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportTier {
    /// First-class: covered by CI on every PR that touches the
    /// relevant paths, complete feature surface.
    Supported,
    /// Builds and runs, partial feature surface, not gated by CI on
    /// every PR (smoke test only).
    Experimental,
    /// Not built for this target.
    Unsupported,
}

/// State of a single capability on the running platform.
///
/// The variants are deliberately distinct so the UI can pick the right
/// affordance — a permission row, an install hint, an experimental
/// badge, or a flat "not on this OS" message — rather than collapsing
/// every failure to "doesn't work".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Capability {
    /// Feature works on this OS without further action.
    Available,
    /// Feature does not exist on this OS or this configuration.
    /// `reason` is meant for the UI tooltip / log line; keep it short.
    Unsupported { reason: String },
    /// Feature exists but the user must grant an OS-level permission
    /// (Accessibility on macOS, etc.). The UI should link to the
    /// matching permission row produced by `PermissionChecker::check`.
    RequiresPermission {
        permission: PermissionKind,
        message: String,
    },
    /// Feature exists but requires an external binary on `$PATH`
    /// (e.g. `wtype` for Wayland auto-paste). The UI should surface
    /// the install hint rather than silently failing.
    RequiresExternalTool {
        tool: String,
        install_hint: Option<String>,
    },
    /// Feature is wired up but not hardened on this platform — for
    /// example the experimental Linux Wayland clipboard or Windows
    /// file-list capture. Callers should still try it; the UI should
    /// flag it so users know regressions are possible.
    Experimental { message: String },
}

impl Capability {
    /// True when the UI may surface the feature *without first asking
    /// the user to do something*.
    ///
    /// `Available` and `Experimental` qualify (the latter just
    /// warrants a warning badge). `RequiresPermission` and
    /// `RequiresExternalTool` both return `false` even though the
    /// feature may flip to usable after the user grants the
    /// permission or installs the tool — the capability layer is
    /// intentionally static and the live state lives in
    /// `PermissionChecker` / the auto-paste path. Pair this with the
    /// live probe before deciding whether to render the feature as
    /// ready, or use [`Self::is_supported_by_platform`] when you only
    /// want to know whether the OS could ever do it.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        matches!(self, Self::Available | Self::Experimental { .. })
    }

    /// True when the running OS could exercise this feature at all,
    /// given any required permission or external tool. Only
    /// `Unsupported` returns `false`. Useful for hiding feature rows
    /// that can never work on this OS — distinct from
    /// [`Self::is_usable`], which also returns `false` while a
    /// permission grant or tool install is still pending.
    #[must_use]
    pub const fn is_supported_by_platform(&self) -> bool {
        !matches!(self, Self::Unsupported { .. })
    }
}

/// Static snapshot of what the host adapter can do.
///
/// Produced by `report_capabilities` in each `nagori-platform-*` crate
/// and aggregated by `nagori-platform-native::capabilities`. Stable
/// enough to serialise over IPC and render in the desktop Settings
/// page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformCapabilities {
    pub platform: Platform,
    pub tier: SupportTier,
    /// Clipboard capture for text payloads (plain + rich where the OS
    /// exposes both). Always the most-supported capability.
    pub capture_text: Capability,
    /// Image clipboard capture (PNG / TIFF / `CF_DIB`). macOS only at
    /// the moment.
    pub capture_image: Capability,
    /// File-list clipboard capture (`CF_HDROP` on Windows, file URLs
    /// on macOS). Surfaced separately from `capture_text` because the
    /// README support matrix lists "Text + files" as a distinct
    /// Windows capability — collapsing it into `capture_text` would
    /// erase information consumers actually want to render.
    pub capture_files: Capability,
    /// Writing text back to the clipboard.
    pub write_text: Capability,
    /// Writing images back to the clipboard. macOS only.
    pub write_image: Capability,
    /// Synthesising Ctrl/Cmd+V into the previous frontmost surface.
    pub auto_paste: Capability,
    /// Registering an in-app global hotkey via
    /// `tauri-plugin-global-shortcut`.
    pub global_hotkey: Capability,
    /// Identifying the application that owned focus before the
    /// palette opened, for refocus + auto-paste targeting.
    pub frontmost_app: Capability,
    /// Whether the OS exposes a permission UI the user can act on
    /// (System Settings on macOS; no-op probes on Windows/Linux).
    pub permissions_ui: Capability,
    /// Whether the bundled updater probe is wired on this platform.
    pub update_check: Capability,
}

/// Capability report for targets nagori does not build for.
///
/// Defined here (rather than behind a `cfg(not(any(target_os = ...)))`
/// guard) so the `nagori-platform-native` aggregator can call it from
/// the same arm shape it uses for the supported targets. Every row is
/// `Unsupported` and the tier is also `Unsupported`, matching what the
/// runtime does on those hosts (`build_native_runtime` returns
/// `AppError::Unsupported`).
#[must_use]
pub fn unsupported_capabilities() -> PlatformCapabilities {
    const REASON: &str = "nagori does not build for this target; only \
         macOS, Windows, and Linux Wayland are supported.";
    let unsupported = || Capability::Unsupported {
        reason: REASON.to_owned(),
    };
    PlatformCapabilities {
        platform: Platform::Unsupported,
        tier: SupportTier::Unsupported,
        capture_text: unsupported(),
        capture_image: unsupported(),
        capture_files: unsupported(),
        write_text: unsupported(),
        write_image: unsupported(),
        auto_paste: unsupported(),
        global_hotkey: unsupported(),
        frontmost_app: unsupported(),
        permissions_ui: unsupported(),
        update_check: unsupported(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_is_usable_for_available_and_experimental() {
        assert!(Capability::Available.is_usable());
        assert!(
            Capability::Experimental {
                message: "x".into()
            }
            .is_usable()
        );
        assert!(!Capability::Unsupported { reason: "x".into() }.is_usable());
        assert!(
            !Capability::RequiresPermission {
                permission: PermissionKind::Accessibility,
                message: "x".into()
            }
            .is_usable()
        );
        assert!(
            !Capability::RequiresExternalTool {
                tool: "wtype".into(),
                install_hint: None
            }
            .is_usable()
        );
    }

    #[test]
    fn unsupported_capabilities_marks_every_row_unsupported() {
        let caps = unsupported_capabilities();
        assert_eq!(caps.platform, Platform::Unsupported);
        assert_eq!(caps.tier, SupportTier::Unsupported);
        for cap in [
            &caps.capture_text,
            &caps.capture_image,
            &caps.capture_files,
            &caps.write_text,
            &caps.write_image,
            &caps.auto_paste,
            &caps.global_hotkey,
            &caps.frontmost_app,
            &caps.permissions_ui,
            &caps.update_check,
        ] {
            assert!(!cap.is_usable());
            assert!(!cap.is_supported_by_platform());
            assert!(matches!(cap, Capability::Unsupported { .. }));
        }
    }

    #[test]
    fn is_supported_by_platform_distinguishes_unsupported_from_setup_required() {
        assert!(Capability::Available.is_supported_by_platform());
        assert!(
            Capability::Experimental {
                message: "x".into()
            }
            .is_supported_by_platform()
        );
        assert!(
            Capability::RequiresPermission {
                permission: PermissionKind::Accessibility,
                message: "x".into()
            }
            .is_supported_by_platform()
        );
        assert!(
            Capability::RequiresExternalTool {
                tool: "wtype".into(),
                install_hint: None
            }
            .is_supported_by_platform()
        );
        assert!(!Capability::Unsupported { reason: "x".into() }.is_supported_by_platform());
    }

    #[test]
    fn platform_serialises_with_natural_names() {
        // Lock in the public JSON contract surfaced over IPC and the
        // `nagori capabilities` CLI. Without the explicit `rename`
        // override the snake_case derive would emit `"mac_o_s"`.
        let cases = [
            (Platform::MacOS, "\"macos\""),
            (Platform::Windows, "\"windows\""),
            (Platform::LinuxWayland, "\"linux_wayland\""),
            (Platform::Unsupported, "\"unsupported\""),
        ];
        for (value, expected) in cases {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
            assert_eq!(
                serde_json::from_str::<Platform>(expected).unwrap(),
                value,
                "round-trip {expected}"
            );
        }
    }

    #[test]
    fn capability_serialises_with_status_tag() {
        let json = serde_json::to_string(&Capability::Available).unwrap();
        assert_eq!(json, r#"{"status":"available"}"#);

        let json = serde_json::to_string(&Capability::RequiresExternalTool {
            tool: "wtype".into(),
            install_hint: Some("apt install wtype".into()),
        })
        .unwrap();
        assert!(json.contains(r#""status":"requires_external_tool""#));
        assert!(json.contains(r#""tool":"wtype""#));
    }
}

use nagori_platform::{
    Capability, PermissionKind, PermissionState, PermissionStatus, Platform, PlatformCapabilities,
    SupportTier,
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionKindDto {
    Accessibility,
    InputMonitoring,
    Clipboard,
    Notifications,
    AutoLaunch,
}

impl From<PermissionKind> for PermissionKindDto {
    fn from(value: PermissionKind) -> Self {
        match value {
            PermissionKind::Accessibility => Self::Accessibility,
            PermissionKind::InputMonitoring => Self::InputMonitoring,
            PermissionKind::Clipboard => Self::Clipboard,
            PermissionKind::Notifications => Self::Notifications,
            PermissionKind::AutoLaunch => Self::AutoLaunch,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStateDto {
    Granted,
    Denied,
    NotDetermined,
    Unsupported,
}

impl From<PermissionState> for PermissionStateDto {
    fn from(value: PermissionState) -> Self {
        match value {
            PermissionState::Granted => Self::Granted,
            PermissionState::Denied => Self::Denied,
            PermissionState::NotDetermined => Self::NotDetermined,
            PermissionState::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatusDto {
    pub kind: PermissionKindDto,
    pub state: PermissionStateDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Stable identifier (e.g. `"accessibility_not_prompted"`) so the
    /// frontend can branch without scraping the message string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Deep-link target inside the Settings window (e.g.
    /// `"setup/accessibility"`) used by the `StatusBar` indicator click
    /// handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_route: Option<String>,
    /// Permalink to the relevant docs section, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
}

impl From<PermissionStatus> for PermissionStatusDto {
    fn from(value: PermissionStatus) -> Self {
        Self {
            kind: value.kind.into(),
            state: value.state.into(),
            message: value.message,
            reason_code: value.reason_code,
            setup_route: value.setup_route,
            docs_url: value.docs_url,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlatformDto {
    // Match the IPC JSON shape (`"macos"`) rather than the camelCase
    // derive's `"macOs"` so the frontend can treat the platform name as
    // a stable identifier across CLI / IPC / Tauri surfaces.
    #[serde(rename = "macos")]
    MacOS,
    Windows,
    LinuxWayland,
    Unsupported,
}

impl From<Platform> for PlatformDto {
    fn from(value: Platform) -> Self {
        match value {
            Platform::MacOS => Self::MacOS,
            Platform::Windows => Self::Windows,
            Platform::LinuxWayland => Self::LinuxWayland,
            Platform::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SupportTierDto {
    Supported,
    Experimental,
    Unsupported,
}

impl From<SupportTier> for SupportTierDto {
    fn from(value: SupportTier) -> Self {
        match value {
            SupportTier::Supported => Self::Supported,
            SupportTier::Experimental => Self::Experimental,
            SupportTier::Unsupported => Self::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CapabilityDto {
    Available,
    Unsupported {
        reason: String,
    },
    RequiresPermission {
        permission: PermissionKindDto,
        message: String,
    },
    RequiresExternalTool {
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        install_hint: Option<String>,
    },
    Experimental {
        message: String,
    },
}

impl From<Capability> for CapabilityDto {
    fn from(value: Capability) -> Self {
        match value {
            Capability::Available => Self::Available,
            Capability::Unsupported { reason } => Self::Unsupported { reason },
            Capability::RequiresPermission {
                permission,
                message,
            } => Self::RequiresPermission {
                permission: permission.into(),
                message,
            },
            Capability::RequiresExternalTool { tool, install_hint } => {
                Self::RequiresExternalTool { tool, install_hint }
            }
            Capability::Experimental { message } => Self::Experimental { message },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilitiesDto {
    pub platform: PlatformDto,
    pub tier: SupportTierDto,
    pub capture_text: CapabilityDto,
    pub capture_image: CapabilityDto,
    pub capture_files: CapabilityDto,
    pub write_text: CapabilityDto,
    pub write_image: CapabilityDto,
    pub clipboard_multi_representation_write: CapabilityDto,
    pub auto_paste: CapabilityDto,
    pub global_hotkey: CapabilityDto,
    pub frontmost_app: CapabilityDto,
    pub permissions_ui: CapabilityDto,
    pub update_check: CapabilityDto,
    pub preview_quick_look: CapabilityDto,
}

impl From<PlatformCapabilities> for PlatformCapabilitiesDto {
    fn from(value: PlatformCapabilities) -> Self {
        Self {
            platform: value.platform.into(),
            tier: value.tier.into(),
            capture_text: value.capture_text.into(),
            capture_image: value.capture_image.into(),
            capture_files: value.capture_files.into(),
            write_text: value.write_text.into(),
            write_image: value.write_image.into(),
            clipboard_multi_representation_write: value.clipboard_multi_representation_write.into(),
            auto_paste: value.auto_paste.into(),
            global_hotkey: value.global_hotkey.into(),
            frontmost_app: value.frontmost_app.into(),
            permissions_ui: value.permissions_ui.into(),
            update_check: value.update_check.into(),
            preview_quick_look: value.preview_quick_look.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn capability_dto_serializes_struct_variant_fields_in_camel_case() {
        // `rename_all = "camelCase"` only touches variant names — without
        // `rename_all_fields` the inner `install_hint` ships as snake_case
        // and silently de-syncs from the TS `installHint?` contract.
        let dto = CapabilityDto::from(nagori_platform::Capability::RequiresExternalTool {
            tool: "wtype".to_owned(),
            install_hint: Some("apt install wtype".to_owned()),
        });
        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(json["status"], json!("requiresExternalTool"));
        assert_eq!(json["tool"], json!("wtype"));
        assert_eq!(json["installHint"], json!("apt install wtype"));
        assert!(
            json.get("install_hint").is_none(),
            "snake_case field should not coexist with camelCase rename"
        );
    }
}

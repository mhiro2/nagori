use std::collections::BTreeMap;

use nagori_core::settings::AiProviderSetting;
use nagori_core::{
    AiOutput, AppSettings, Appearance, ClipboardContent, ClipboardEntry, ContentKind, EntryId,
    Locale, PaletteHotkeyAction, PasteFormat, RankReason, RecentOrder, RepresentationRole,
    SearchFilters, SearchMode, SearchResult, SecondaryHotkeyAction, SecretHandling, Sensitivity,
    StoredClipboardRepresentation, UpdateChannel, is_text_safe_for_default_output,
    safe_preview_for_dto,
};
use nagori_platform::{
    Capability, PermissionKind, PermissionState, PermissionStatus, Platform, PlatformCapabilities,
    SupportTier,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentKindDto {
    Text,
    Url,
    Code,
    Image,
    FileList,
    RichText,
    Unknown,
}

impl From<ContentKind> for ContentKindDto {
    fn from(kind: ContentKind) -> Self {
        match kind {
            ContentKind::Text => Self::Text,
            ContentKind::Url => Self::Url,
            ContentKind::Code => Self::Code,
            ContentKind::Image => Self::Image,
            ContentKind::FileList => Self::FileList,
            ContentKind::RichText => Self::RichText,
            ContentKind::Unknown => Self::Unknown,
        }
    }
}

impl From<ContentKindDto> for ContentKind {
    fn from(kind: ContentKindDto) -> Self {
        match kind {
            ContentKindDto::Text => Self::Text,
            ContentKindDto::Url => Self::Url,
            ContentKindDto::Code => Self::Code,
            ContentKindDto::Image => Self::Image,
            ContentKindDto::FileList => Self::FileList,
            ContentKindDto::RichText => Self::RichText,
            ContentKindDto::Unknown => Self::Unknown,
        }
    }
}

fn default_capture_kind_dtos() -> Vec<ContentKindDto> {
    nagori_core::settings::default_capture_kinds()
        .into_iter()
        .map(Into::into)
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RepresentationRoleDto {
    Primary,
    PlainFallback,
    Alternative,
}

impl From<RepresentationRole> for RepresentationRoleDto {
    fn from(role: RepresentationRole) -> Self {
        match role {
            RepresentationRole::Primary => Self::Primary,
            RepresentationRole::PlainFallback => Self::PlainFallback,
            RepresentationRole::Alternative => Self::Alternative,
        }
    }
}

/// Wire-safe projection of one preserved representation row. Mirrors
/// `nagori_ipc::RepresentationSummaryDto` but serialises in camelCase so
/// the Svelte side can consume the field without a transformation layer.
/// Bytes/text stay daemon-side; only the MIME type, role, and byte count
/// reach the renderer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepresentationSummaryDto {
    pub mime_type: String,
    pub role: RepresentationRoleDto,
    pub byte_count: u64,
}

impl RepresentationSummaryDto {
    pub fn from_stored(rep: &StoredClipboardRepresentation) -> Self {
        Self {
            mime_type: rep.mime_type.clone(),
            role: rep.role.into(),
            byte_count: rep.byte_count() as u64,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub preview: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<OffsetDateTime>,
    pub use_count: u32,
    pub pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    pub sensitivity: Sensitivity,
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl EntryDto {
    pub fn from_entry(entry: ClipboardEntry, include_text: bool) -> Self {
        let preview = safe_preview_for_dto(&entry);
        Self {
            id: entry.id,
            kind: entry.content_kind().into(),
            text: include_text.then(|| entry.plain_text().unwrap_or_default().to_owned()),
            preview,
            created_at: entry.metadata.created_at,
            updated_at: entry.metadata.updated_at,
            last_used_at: entry.metadata.last_used_at,
            use_count: entry.metadata.use_count,
            pinned: entry.lifecycle.pinned,
            source_app_name: entry.metadata.source.and_then(|source| source.name),
            sensitivity: entry.sensitivity,
            representation_summary: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_representation_summary(
        mut self,
        representations: &[StoredClipboardRepresentation],
    ) -> Self {
        self.representation_summary = representations
            .iter()
            .map(RepresentationSummaryDto::from_stored)
            .collect();
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    pub preview: String,
    pub score: f32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub pinned: bool,
    pub sensitivity: Sensitivity,
    pub rank_reasons: Vec<RankReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_app_name: Option<String>,
    pub representation_summary: Vec<RepresentationSummaryDto>,
}

impl From<SearchResult> for SearchResultDto {
    fn from(value: SearchResult) -> Self {
        Self {
            id: value.entry_id,
            kind: value.content_kind.into(),
            preview: value.preview,
            score: value.score,
            created_at: value.created_at,
            pinned: value.pinned,
            sensitivity: value.sensitivity,
            rank_reasons: value.rank_reason,
            source_app_name: value.source_app_name,
            representation_summary: Vec::new(),
        }
    }
}

impl SearchResultDto {
    #[must_use]
    pub fn with_representation_summary(
        mut self,
        representations: &[StoredClipboardRepresentation],
    ) -> Self {
        self.representation_summary = representations
            .iter()
            .map(RepresentationSummaryDto::from_stored)
            .collect();
        self
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFiltersDto {
    #[serde(default)]
    pub kinds: Vec<ContentKindDto>,
    #[serde(default)]
    pub pinned_only: bool,
    #[serde(default)]
    pub source_app: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_after: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_before: Option<OffsetDateTime>,
}

impl From<SearchFiltersDto> for SearchFilters {
    fn from(value: SearchFiltersDto) -> Self {
        Self {
            kinds: value.kinds.into_iter().map(Into::into).collect(),
            pinned_only: value.pinned_only,
            source_app: value.source_app,
            created_after: value.created_after,
            created_before: value.created_before,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequestDto {
    pub query: String,
    #[serde(default)]
    pub mode: Option<SearchMode>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub filters: Option<SearchFiltersDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponseDto {
    pub results: Vec<SearchResultDto>,
    pub total_candidates: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPreviewDto {
    pub id: EntryId,
    pub kind: ContentKindDto,
    pub title: Option<String>,
    pub preview_text: String,
    pub body: PreviewBodyDto,
    pub metadata: EntryPreviewMetadataDto,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPreviewMetadataDto {
    pub byte_count: usize,
    pub char_count: usize,
    pub line_count: usize,
    pub truncated: bool,
    pub sensitive: bool,
    pub full_content_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PreviewBodyDto {
    Text {
        text: String,
    },
    Code {
        text: String,
        language: Option<String>,
    },
    Url {
        url: String,
        domain: Option<String>,
    },
    Image {
        mime_type: Option<String>,
        byte_count: usize,
        width: Option<u32>,
        height: Option<u32>,
    },
    FileList {
        paths: Vec<String>,
    },
    RichText {
        text: String,
    },
    Unknown {
        text: String,
    },
}

impl EntryPreviewDto {
    pub fn from_entry(entry: &ClipboardEntry) -> Self {
        const MAX_PREVIEW_BYTES: usize = 128 * 1024;

        let sensitive = !is_text_safe_for_default_output(entry.sensitivity);
        let raw_text = if sensitive {
            safe_preview_for_dto(entry)
        } else {
            entry.plain_text().unwrap_or_default().to_owned()
        };
        let (preview_text, truncated) = truncate_utf8(&raw_text, MAX_PREVIEW_BYTES);
        let full_content_available =
            !sensitive && !matches!(entry.sensitivity, Sensitivity::Blocked);
        let title = entry.search.title.clone();
        let language = entry.search.language.clone();
        let domain = match &entry.content {
            ClipboardContent::Url(value) => value.domain.clone(),
            _ => None,
        };
        let body = if sensitive {
            PreviewBodyDto::Text {
                text: preview_text.clone(),
            }
        } else {
            match &entry.content {
                ClipboardContent::Text(_) => PreviewBodyDto::Text {
                    text: preview_text.clone(),
                },
                ClipboardContent::Code(value) => PreviewBodyDto::Code {
                    text: preview_text.clone(),
                    language: value.language_hint.clone().or_else(|| language.clone()),
                },
                ClipboardContent::Url(value) => PreviewBodyDto::Url {
                    url: preview_text.clone(),
                    domain: value.domain.clone(),
                },
                ClipboardContent::Image(value) => PreviewBodyDto::Image {
                    mime_type: value.mime_type.clone(),
                    byte_count: value.byte_count,
                    width: value.width,
                    height: value.height,
                },
                ClipboardContent::FileList(value) => PreviewBodyDto::FileList {
                    paths: value.paths.iter().take(50).cloned().collect(),
                },
                ClipboardContent::RichText(_) => PreviewBodyDto::RichText {
                    text: preview_text.clone(),
                },
                ClipboardContent::Unknown(_) => PreviewBodyDto::Unknown {
                    text: preview_text.clone(),
                },
            }
        };
        Self {
            id: entry.id,
            kind: entry.content_kind().into(),
            title,
            preview_text,
            body,
            metadata: EntryPreviewMetadataDto {
                byte_count: raw_text.len(),
                char_count: raw_text.chars().count(),
                line_count: raw_text.lines().count().max(1),
                truncated,
                sensitive,
                full_content_available,
                domain,
                language,
            },
        }
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = value[..end].to_owned();
    out.push('…');
    (out, true)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiActionResultDto {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_entry_id: Option<EntryId>,
    pub warnings: Vec<String>,
}

impl From<AiOutput> for AiActionResultDto {
    fn from(value: AiOutput) -> Self {
        Self {
            text: value.text,
            created_entry_id: value.created_entry,
            warnings: value.warnings,
        }
    }
}

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
}

impl From<PermissionStatus> for PermissionStatusDto {
    fn from(value: PermissionStatus) -> Self {
        Self {
            kind: value.kind.into(),
            state: value.state.into(),
            message: value.message,
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
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AiProviderSettingDto {
    None,
    Local,
    Remote { name: String },
}

impl From<AiProviderSetting> for AiProviderSettingDto {
    fn from(value: AiProviderSetting) -> Self {
        match value {
            AiProviderSetting::None => Self::None,
            AiProviderSetting::Local => Self::Local,
            AiProviderSetting::Remote { name } => Self::Remote { name },
        }
    }
}

impl From<AiProviderSettingDto> for AiProviderSetting {
    fn from(value: AiProviderSettingDto) -> Self {
        match value {
            AiProviderSettingDto::None => Self::None,
            AiProviderSettingDto::Local => Self::Local,
            AiProviderSettingDto::Remote { name } => Self::Remote { name },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum LocaleDto {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "ko")]
    Ko,
    #[serde(rename = "zh-Hans")]
    ZhHans,
    #[serde(rename = "zh-Hant")]
    ZhHant,
    #[serde(rename = "de")]
    De,
    #[serde(rename = "fr")]
    Fr,
    #[serde(rename = "es")]
    Es,
}

impl From<Locale> for LocaleDto {
    fn from(value: Locale) -> Self {
        match value {
            Locale::System => Self::System,
            Locale::En => Self::En,
            Locale::Ja => Self::Ja,
            Locale::Ko => Self::Ko,
            Locale::ZhHans => Self::ZhHans,
            Locale::ZhHant => Self::ZhHant,
            Locale::De => Self::De,
            Locale::Fr => Self::Fr,
            Locale::Es => Self::Es,
        }
    }
}

impl From<LocaleDto> for Locale {
    fn from(value: LocaleDto) -> Self {
        match value {
            LocaleDto::System => Self::System,
            LocaleDto::En => Self::En,
            LocaleDto::Ja => Self::Ja,
            LocaleDto::Ko => Self::Ko,
            LocaleDto::ZhHans => Self::ZhHans,
            LocaleDto::ZhHant => Self::ZhHant,
            LocaleDto::De => Self::De,
            LocaleDto::Fr => Self::Fr,
            LocaleDto::Es => Self::Es,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretHandlingDto {
    Block,
    StoreRedacted,
    StoreFull,
}

impl From<SecretHandling> for SecretHandlingDto {
    fn from(value: SecretHandling) -> Self {
        match value {
            SecretHandling::Block => Self::Block,
            SecretHandling::StoreRedacted => Self::StoreRedacted,
            SecretHandling::StoreFull => Self::StoreFull,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteFormatDto {
    Preserve,
    PlainText,
}

impl From<PasteFormat> for PasteFormatDto {
    fn from(value: PasteFormat) -> Self {
        match value {
            PasteFormat::Preserve => Self::Preserve,
            PasteFormat::PlainText => Self::PlainText,
        }
    }
}

impl From<PasteFormatDto> for PasteFormat {
    fn from(value: PasteFormatDto) -> Self {
        match value {
            PasteFormatDto::Preserve => Self::Preserve,
            PasteFormatDto::PlainText => Self::PlainText,
        }
    }
}

impl Default for PasteFormatDto {
    fn default() -> Self {
        PasteFormat::default().into()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecentOrderDto {
    ByRecency,
    ByUseCount,
    PinnedFirstThenRecency,
}

impl From<RecentOrder> for RecentOrderDto {
    fn from(value: RecentOrder) -> Self {
        match value {
            RecentOrder::ByRecency => Self::ByRecency,
            RecentOrder::ByUseCount => Self::ByUseCount,
            RecentOrder::PinnedFirstThenRecency => Self::PinnedFirstThenRecency,
        }
    }
}

impl From<RecentOrderDto> for RecentOrder {
    fn from(value: RecentOrderDto) -> Self {
        match value {
            RecentOrderDto::ByRecency => Self::ByRecency,
            RecentOrderDto::ByUseCount => Self::ByUseCount,
            RecentOrderDto::PinnedFirstThenRecency => Self::PinnedFirstThenRecency,
        }
    }
}

impl Default for RecentOrderDto {
    fn default() -> Self {
        RecentOrder::default().into()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceDto {
    Light,
    Dark,
    System,
}

impl From<Appearance> for AppearanceDto {
    fn from(value: Appearance) -> Self {
        match value {
            Appearance::Light => Self::Light,
            Appearance::Dark => Self::Dark,
            Appearance::System => Self::System,
        }
    }
}

impl From<AppearanceDto> for Appearance {
    fn from(value: AppearanceDto) -> Self {
        match value {
            AppearanceDto::Light => Self::Light,
            AppearanceDto::Dark => Self::Dark,
            AppearanceDto::System => Self::System,
        }
    }
}

impl Default for AppearanceDto {
    fn default() -> Self {
        Appearance::default().into()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannelDto {
    Stable,
}

impl From<UpdateChannel> for UpdateChannelDto {
    fn from(value: UpdateChannel) -> Self {
        match value {
            UpdateChannel::Stable => Self::Stable,
        }
    }
}

impl From<UpdateChannelDto> for UpdateChannel {
    fn from(value: UpdateChannelDto) -> Self {
        match value {
            UpdateChannelDto::Stable => Self::Stable,
        }
    }
}

impl Default for UpdateChannelDto {
    fn default() -> Self {
        UpdateChannel::default().into()
    }
}

impl From<SecretHandlingDto> for SecretHandling {
    fn from(value: SecretHandlingDto) -> Self {
        match value {
            SecretHandlingDto::Block => Self::Block,
            SecretHandlingDto::StoreRedacted => Self::StoreRedacted,
            SecretHandlingDto::StoreFull => Self::StoreFull,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsDto {
    pub global_hotkey: String,
    pub history_retention_count: usize,
    pub history_retention_days: Option<u32>,
    pub max_entry_size_bytes: usize,
    #[serde(default = "default_capture_kind_dtos")]
    pub capture_kinds: Vec<ContentKindDto>,
    pub max_total_bytes: Option<u64>,
    pub capture_enabled: bool,
    pub auto_paste_enabled: bool,
    #[serde(default)]
    pub paste_format_default: PasteFormatDto,
    pub paste_delay_ms: u64,
    pub app_denylist: Vec<String>,
    pub regex_denylist: Vec<String>,
    pub local_only_mode: bool,
    pub ai_provider: AiProviderSettingDto,
    pub ai_enabled: bool,
    pub semantic_search_enabled: bool,
    pub cli_ipc_enabled: bool,
    pub locale: LocaleDto,
    #[serde(default)]
    pub recent_order: RecentOrderDto,
    #[serde(default)]
    pub appearance: AppearanceDto,
    pub auto_launch: bool,
    #[serde(default)]
    pub secret_handling: SecretHandlingDto,
    #[serde(default)]
    pub palette_hotkeys: BTreeMap<PaletteHotkeyAction, String>,
    #[serde(default)]
    pub secondary_hotkeys: BTreeMap<SecondaryHotkeyAction, String>,
    #[serde(default = "nagori_core::settings::default_palette_row_count")]
    pub palette_row_count: u32,
    #[serde(default = "nagori_core::settings::default_show_preview_pane")]
    pub show_preview_pane: bool,
    #[serde(default = "nagori_core::settings::default_show_in_menu_bar")]
    pub show_in_menu_bar: bool,
    #[serde(default)]
    pub clear_on_quit: bool,
    #[serde(default = "nagori_core::settings::default_capture_initial_clipboard_on_launch")]
    pub capture_initial_clipboard_on_launch: bool,
    #[serde(default = "nagori_core::settings::default_auto_update_check")]
    pub auto_update_check: bool,
    #[serde(default)]
    pub update_channel: UpdateChannelDto,
}

impl Default for SecretHandlingDto {
    fn default() -> Self {
        SecretHandling::default().into()
    }
}

impl From<AppSettings> for AppSettingsDto {
    fn from(value: AppSettings) -> Self {
        Self {
            global_hotkey: value.global_hotkey,
            history_retention_count: value.history_retention_count,
            history_retention_days: value.history_retention_days,
            max_entry_size_bytes: value.max_entry_size_bytes,
            capture_kinds: value.capture_kinds.into_iter().map(Into::into).collect(),
            max_total_bytes: value.max_total_bytes,
            capture_enabled: value.capture_enabled,
            auto_paste_enabled: value.auto_paste_enabled,
            paste_format_default: value.paste_format_default.into(),
            paste_delay_ms: value.paste_delay_ms,
            app_denylist: value.app_denylist,
            regex_denylist: value.regex_denylist,
            local_only_mode: value.local_only_mode,
            ai_provider: value.ai_provider.into(),
            ai_enabled: value.ai_enabled,
            semantic_search_enabled: value.semantic_search_enabled,
            cli_ipc_enabled: value.cli_ipc_enabled,
            locale: value.locale.into(),
            recent_order: value.recent_order.into(),
            appearance: value.appearance.into(),
            auto_launch: value.auto_launch,
            secret_handling: value.secret_handling.into(),
            palette_hotkeys: value.palette_hotkeys,
            secondary_hotkeys: value.secondary_hotkeys,
            palette_row_count: value.palette_row_count,
            show_preview_pane: value.show_preview_pane,
            show_in_menu_bar: value.show_in_menu_bar,
            clear_on_quit: value.clear_on_quit,
            capture_initial_clipboard_on_launch: value.capture_initial_clipboard_on_launch,
            auto_update_check: value.auto_update_check,
            update_channel: value.update_channel.into(),
        }
    }
}

impl From<AppSettingsDto> for AppSettings {
    fn from(value: AppSettingsDto) -> Self {
        Self {
            global_hotkey: value.global_hotkey,
            history_retention_count: value.history_retention_count,
            history_retention_days: value.history_retention_days,
            max_entry_size_bytes: value.max_entry_size_bytes,
            capture_kinds: value.capture_kinds.into_iter().map(Into::into).collect(),
            max_total_bytes: value.max_total_bytes,
            capture_enabled: value.capture_enabled,
            auto_paste_enabled: value.auto_paste_enabled,
            paste_format_default: value.paste_format_default.into(),
            paste_delay_ms: value.paste_delay_ms,
            app_denylist: value.app_denylist,
            regex_denylist: value.regex_denylist,
            local_only_mode: value.local_only_mode,
            ai_provider: value.ai_provider.into(),
            ai_enabled: value.ai_enabled,
            semantic_search_enabled: value.semantic_search_enabled,
            cli_ipc_enabled: value.cli_ipc_enabled,
            locale: value.locale.into(),
            recent_order: value.recent_order.into(),
            appearance: value.appearance.into(),
            auto_launch: value.auto_launch,
            secret_handling: value.secret_handling.into(),
            palette_hotkeys: value.palette_hotkeys,
            secondary_hotkeys: value.secondary_hotkeys,
            palette_row_count: value.palette_row_count,
            show_preview_pane: value.show_preview_pane,
            show_in_menu_bar: value.show_in_menu_bar,
            clear_on_quit: value.clear_on_quit,
            capture_initial_clipboard_on_launch: value.capture_initial_clipboard_on_launch,
            auto_update_check: value.auto_update_check,
            update_channel: value.update_channel.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use nagori_core::{
        AppSettings, Appearance, ClipboardData, ClipboardRepresentation, ClipboardSnapshot,
        ContentKind, EntryFactory, PasteFormat, RecentOrder, SecretHandling, Sensitivity,
        UpdateChannel,
    };
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    fn text_entry(body: &str) -> nagori_core::ClipboardEntry {
        EntryFactory::from_text(body)
    }

    fn image_entry(bytes: Vec<u8>) -> nagori_core::ClipboardEntry {
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash(
                nagori_core::ContentHash::sha256(&bytes).value,
            ),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(bytes),
            }],
        };
        EntryFactory::from_snapshot(snapshot).expect("png snapshot should produce entry")
    }

    #[test]
    fn entry_preview_for_secret_text_only_exposes_redacted_preview() {
        let mut entry = text_entry("ghp_abcdefghijklmnopqrstuvwxyz1234567890");
        entry.search.preview = "[REDACTED]".to_owned();
        entry.sensitivity = Sensitivity::Secret;

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.sensitive);
        assert!(!dto.metadata.full_content_available);
        match dto.body {
            PreviewBodyDto::Text { ref text } => assert_eq!(text, "[REDACTED]"),
            other => panic!("expected redacted Text body, got {other:?}"),
        }
        assert_eq!(dto.preview_text, "[REDACTED]");
    }

    #[test]
    fn entry_preview_for_private_text_uses_preview_only() {
        let mut entry = text_entry("482915");
        entry.search.preview = "(redacted OTP)".to_owned();
        entry.sensitivity = Sensitivity::Private;

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.sensitive);
        match dto.body {
            PreviewBodyDto::Text { ref text } => assert_eq!(text, "(redacted OTP)"),
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_blocked_replaces_preview_with_placeholder() {
        // The classifier never sets `redacted_preview` for Blocked, so the
        // stored `search.preview` is still raw-derived. The DTO must
        // substitute the placeholder rather than surfacing whatever was on
        // the row, even when callers set it to a benign-looking string.
        let mut entry = text_entry("blocked clip");
        entry.search.preview = "raw secret value".to_owned();
        entry.sensitivity = Sensitivity::Blocked;

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.sensitive);
        assert!(!dto.metadata.full_content_available);
        match dto.body {
            PreviewBodyDto::Text { text } => {
                assert_eq!(text, nagori_core::BLOCKED_PREVIEW_PLACEHOLDER);
            }
            other => panic!("expected Text body, got {other:?}"),
        }
    }

    #[test]
    fn entry_preview_for_image_returns_image_body_with_byte_count() {
        // The PNG magic prefix is required: `EntryFactory::from_snapshot`
        // drops image representations whose bytes don't match the
        // declared MIME, so a fake byte string would be rejected by the
        // capture-time signature gate before this test could observe a
        // preview.
        let bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0xCA, 0xFE];
        let entry = image_entry(bytes.clone());

        let dto = EntryPreviewDto::from_entry(&entry);
        match dto.body {
            PreviewBodyDto::Image {
                mime_type,
                byte_count,
                width,
                height,
            } => {
                assert_eq!(mime_type.as_deref(), Some("image/png"));
                assert_eq!(byte_count, bytes.len());
                assert_eq!(width, None);
                assert_eq!(height, None);
            }
            other => panic!("expected Image body, got {other:?}"),
        }
        assert!(matches!(dto.kind, ContentKindDto::Image));
    }

    #[test]
    fn entry_preview_truncates_oversized_text_bodies() {
        // 200 KiB exceeds the 128 KiB preview cap; body must end with the
        // ellipsis sentinel and `truncated` must be set.
        let huge: String = "a".repeat(200 * 1024);
        let entry = text_entry(&huge);

        let dto = EntryPreviewDto::from_entry(&entry);
        assert!(dto.metadata.truncated);
        assert!(dto.preview_text.ends_with('…'));
        assert!(dto.preview_text.len() <= 128 * 1024 + '…'.len_utf8());
    }

    #[test]
    fn app_settings_dto_round_trip_preserves_every_field() {
        use nagori_core::{PaletteHotkeyAction, SecondaryHotkeyAction};
        use std::collections::BTreeMap;
        // Pin every field so a future addition that forgets one of the
        // conversion arms (camelCase serde rename, secret_handling default,
        // ai_provider variants, locale tag) trips this test.
        let mut palette_hotkeys = BTreeMap::new();
        palette_hotkeys.insert(PaletteHotkeyAction::Pin, "Cmd+Alt+P".to_owned());
        let mut secondary_hotkeys = BTreeMap::new();
        secondary_hotkeys.insert(SecondaryHotkeyAction::RepasteLast, "Cmd+Alt+V".to_owned());

        let original = AppSettings {
            global_hotkey: "Cmd+Shift+V".to_owned(),
            history_retention_count: 1234,
            history_retention_days: Some(7),
            max_entry_size_bytes: 2 * 1024 * 1024,
            capture_kinds: [ContentKind::Text, ContentKind::Image]
                .into_iter()
                .collect(),
            max_total_bytes: Some(64 * 1024 * 1024),
            capture_enabled: false,
            auto_paste_enabled: true,
            paste_format_default: PasteFormat::PlainText,
            paste_delay_ms: 80,
            app_denylist: vec!["1Password".to_owned(), "Bitwarden".to_owned()],
            regex_denylist: vec!["INTERNAL-\\d+".to_owned()],
            local_only_mode: true,
            ai_provider: AiProviderSetting::Remote {
                name: "anthropic".to_owned(),
            },
            ai_enabled: true,
            semantic_search_enabled: true,
            cli_ipc_enabled: false,
            locale: nagori_core::Locale::Ja,
            recent_order: RecentOrder::ByUseCount,
            appearance: Appearance::Dark,
            auto_launch: true,
            secret_handling: SecretHandling::StoreFull,
            palette_hotkeys: palette_hotkeys.clone(),
            secondary_hotkeys: secondary_hotkeys.clone(),
            palette_row_count: 12,
            show_preview_pane: false,
            show_in_menu_bar: false,
            clear_on_quit: true,
            capture_initial_clipboard_on_launch: false,
            auto_update_check: false,
            update_channel: UpdateChannel::Stable,
        };

        let dto: AppSettingsDto = original.clone().into();
        let restored: AppSettings = dto.into();
        assert_eq!(restored.global_hotkey, original.global_hotkey);
        assert_eq!(
            restored.history_retention_count,
            original.history_retention_count
        );
        assert_eq!(
            restored.history_retention_days,
            original.history_retention_days
        );
        assert_eq!(restored.max_entry_size_bytes, original.max_entry_size_bytes);
        assert_eq!(restored.capture_kinds, original.capture_kinds);
        assert_eq!(restored.max_total_bytes, original.max_total_bytes);
        assert_eq!(restored.capture_enabled, original.capture_enabled);
        assert_eq!(restored.auto_paste_enabled, original.auto_paste_enabled);
        assert_eq!(restored.paste_format_default, original.paste_format_default);
        assert_eq!(restored.paste_delay_ms, original.paste_delay_ms);
        assert_eq!(restored.app_denylist, original.app_denylist);
        assert_eq!(restored.regex_denylist, original.regex_denylist);
        assert_eq!(restored.local_only_mode, original.local_only_mode);
        assert!(matches!(
            restored.ai_provider,
            AiProviderSetting::Remote { ref name } if name == "anthropic",
        ));
        assert_eq!(restored.ai_enabled, original.ai_enabled);
        assert_eq!(
            restored.semantic_search_enabled,
            original.semantic_search_enabled
        );
        assert_eq!(restored.cli_ipc_enabled, original.cli_ipc_enabled);
        assert!(matches!(restored.locale, nagori_core::Locale::Ja));
        assert!(matches!(restored.recent_order, RecentOrder::ByUseCount));
        assert!(matches!(restored.appearance, Appearance::Dark));
        assert_eq!(restored.auto_launch, original.auto_launch);
        assert!(matches!(
            restored.secret_handling,
            SecretHandling::StoreFull
        ));
        assert_eq!(restored.palette_hotkeys, palette_hotkeys);
        assert_eq!(restored.secondary_hotkeys, secondary_hotkeys);
        assert_eq!(restored.palette_row_count, 12);
        assert!(!restored.show_preview_pane);
        assert!(!restored.show_in_menu_bar);
        assert!(restored.clear_on_quit);
        assert!(!restored.capture_initial_clipboard_on_launch);
        assert!(!restored.auto_update_check);
        assert!(matches!(restored.update_channel, UpdateChannel::Stable));
    }

    #[test]
    fn app_settings_dto_serializes_secret_handling_as_snake_case() {
        // The Tauri command boundary speaks JSON — the Svelte side reads
        // `secret_handling: "store_redacted"`, so the snake_case rename must
        // survive any future churn on the enum.
        let dto: AppSettingsDto = AppSettings::default().into();
        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(json["secretHandling"], json!("store_redacted"));
        assert_eq!(json["aiProvider"], json!("none"));
        assert_eq!(json["locale"], json!("system"));
        assert_eq!(json["pasteFormatDefault"], json!("preserve"));
        assert_eq!(json["recentOrder"], json!("by_recency"));
        assert_eq!(json["appearance"], json!("system"));
    }

    #[test]
    fn locale_dto_wire_tag_is_stable_for_every_variant() {
        // The frontend parses the locale tag verbatim — a typo in a serde
        // rename would silently drop a locale even though the type-level
        // `From` arms still match. Pin the wire format for every variant.
        let cases: &[(nagori_core::Locale, &str)] = &[
            (nagori_core::Locale::System, "system"),
            (nagori_core::Locale::En, "en"),
            (nagori_core::Locale::Ja, "ja"),
            (nagori_core::Locale::Ko, "ko"),
            (nagori_core::Locale::ZhHans, "zh-Hans"),
            (nagori_core::Locale::ZhHant, "zh-Hant"),
            (nagori_core::Locale::De, "de"),
            (nagori_core::Locale::Fr, "fr"),
            (nagori_core::Locale::Es, "es"),
        ];
        for (locale, expected) in cases {
            let dto: LocaleDto = (*locale).into();
            let serialized = serde_json::to_value(dto).expect("serialize");
            assert_eq!(serialized, json!(expected), "wire tag for {locale:?}");
            let parsed: LocaleDto = serde_json::from_value(json!(expected)).expect("deserialize");
            let round_tripped: nagori_core::Locale = parsed.into();
            assert_eq!(round_tripped, *locale, "round-trip for {locale:?}");
        }
    }

    #[test]
    fn entry_preview_for_url_emits_url_body_with_domain() {
        // URL-shaped clips should round-trip the parsed domain so the
        // frontend can render the badged preview without re-parsing.
        let snapshot = ClipboardSnapshot {
            sequence: nagori_core::ClipboardSequence::content_hash(
                nagori_core::ContentHash::sha256(b"https://example.com/foo").value,
            ),
            captured_at: OffsetDateTime::now_utc(),
            source: None,
            representations: vec![ClipboardRepresentation {
                mime_type: "text/plain".to_owned(),
                data: ClipboardData::Text("https://example.com/foo?bar=1".to_owned()),
            }],
        };
        let entry = EntryFactory::from_snapshot(snapshot).expect("url snapshot");
        let dto = EntryPreviewDto::from_entry(&entry);
        match dto.body {
            PreviewBodyDto::Url { url, domain } => {
                assert!(url.contains("example.com"));
                assert_eq!(domain.as_deref(), Some("example.com"));
            }
            other => panic!("expected Url body, got {other:?}"),
        }
    }

    #[test]
    fn entry_dto_omits_text_for_private_or_secret_unless_caller_opts_in() {
        // `EntryDto::from_entry` exposes the `include_text` flag so the
        // command layer can keep raw bodies out of the default response shape
        // for sensitive entries while still returning text on copy/paste paths.
        let mut entry = text_entry("super secret value");
        entry.sensitivity = Sensitivity::Secret;

        let stripped = EntryDto::from_entry(entry.clone(), false);
        assert!(stripped.text.is_none());
        let with_text = EntryDto::from_entry(entry, true);
        assert_eq!(with_text.text.as_deref(), Some("super secret value"));
    }

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

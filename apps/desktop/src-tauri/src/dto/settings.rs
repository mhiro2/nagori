use std::collections::BTreeMap;

use nagori_core::settings::OnboardingSettings;
use nagori_core::{
    AiActionId, AiProviderKind, AiSettings, AppDenyRule, AppSettings, Appearance, Locale,
    PaletteHotkeyAction, PasteFormat, RecentOrder, RuleSource, SecondaryHotkeyAction,
    SecretHandling, SourceAppIdKind, UpdateChannel,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{ContentKindDto, default_capture_kind_dtos};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AiProviderKindDto {
    #[default]
    Disabled,
    AppleNative,
    OpenAiCompatible,
}

impl From<AiProviderKind> for AiProviderKindDto {
    fn from(value: AiProviderKind) -> Self {
        match value {
            AiProviderKind::Disabled => Self::Disabled,
            AiProviderKind::AppleNative => Self::AppleNative,
            AiProviderKind::OpenAiCompatible => Self::OpenAiCompatible,
        }
    }
}

impl From<AiProviderKindDto> for AiProviderKind {
    fn from(value: AiProviderKindDto) -> Self {
        match value {
            AiProviderKindDto::Disabled => Self::Disabled,
            AiProviderKindDto::AppleNative => Self::AppleNative,
            AiProviderKindDto::OpenAiCompatible => Self::OpenAiCompatible,
        }
    }
}

/// Wire shape of [`AiSettings`]. The renderer drives the AI settings tab and
/// the palette's availability gating off this.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsDto {
    pub enabled: bool,
    pub provider: AiProviderKindDto,
    pub allowed_actions: Vec<AiActionId>,
    pub allow_streaming: bool,
    pub request_timeout_ms: u64,
    pub semantic_index_enabled: bool,
    pub semantic_index_ac_power_only: bool,
    pub onboarding_dismissed: bool,
    pub allow_openai_fallback_prompt: bool,
}

impl From<AiSettings> for AiSettingsDto {
    fn from(value: AiSettings) -> Self {
        Self {
            enabled: value.enabled,
            provider: value.provider.into(),
            allowed_actions: value.allowed_actions,
            allow_streaming: value.allow_streaming,
            request_timeout_ms: value.request_timeout_ms,
            semantic_index_enabled: value.semantic_index_enabled,
            semantic_index_ac_power_only: value.semantic_index_ac_power_only,
            onboarding_dismissed: value.onboarding_dismissed,
            allow_openai_fallback_prompt: value.allow_openai_fallback_prompt,
        }
    }
}

impl From<AiSettingsDto> for AiSettings {
    fn from(value: AiSettingsDto) -> Self {
        Self {
            enabled: value.enabled,
            provider: value.provider.into(),
            allowed_actions: value.allowed_actions,
            allow_streaming: value.allow_streaming,
            request_timeout_ms: value.request_timeout_ms,
            semantic_index_enabled: value.semantic_index_enabled,
            semantic_index_ac_power_only: value.semantic_index_ac_power_only,
            onboarding_dismissed: value.onboarding_dismissed,
            allow_openai_fallback_prompt: value.allow_openai_fallback_prompt,
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

/// Wire shape of [`SourceAppIdKind`]. Kept in lockstep with the core
/// enum; the frontend renders the kind as part of a denylist row so
/// the user can distinguish a bundle-ID rule from an exe-name rule.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAppIdKindDto {
    MacosBundleId,
    WindowsExeName,
    WindowsExecutablePath,
    LinuxDesktopId,
    LinuxFlatpakId,
    X11WmClass,
}

impl From<SourceAppIdKind> for SourceAppIdKindDto {
    fn from(value: SourceAppIdKind) -> Self {
        match value {
            SourceAppIdKind::MacosBundleId => Self::MacosBundleId,
            SourceAppIdKind::WindowsExeName => Self::WindowsExeName,
            SourceAppIdKind::WindowsExecutablePath => Self::WindowsExecutablePath,
            SourceAppIdKind::LinuxDesktopId => Self::LinuxDesktopId,
            SourceAppIdKind::LinuxFlatpakId => Self::LinuxFlatpakId,
            SourceAppIdKind::X11WmClass => Self::X11WmClass,
        }
    }
}

impl From<SourceAppIdKindDto> for SourceAppIdKind {
    fn from(value: SourceAppIdKindDto) -> Self {
        match value {
            SourceAppIdKindDto::MacosBundleId => Self::MacosBundleId,
            SourceAppIdKindDto::WindowsExeName => Self::WindowsExeName,
            SourceAppIdKindDto::WindowsExecutablePath => Self::WindowsExecutablePath,
            SourceAppIdKindDto::LinuxDesktopId => Self::LinuxDesktopId,
            SourceAppIdKindDto::LinuxFlatpakId => Self::LinuxFlatpakId,
            SourceAppIdKindDto::X11WmClass => Self::X11WmClass,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSourceDto {
    #[default]
    Manual,
    Preset,
}

impl From<RuleSource> for RuleSourceDto {
    fn from(value: RuleSource) -> Self {
        match value {
            RuleSource::Manual => Self::Manual,
            RuleSource::Preset => Self::Preset,
        }
    }
}

impl From<RuleSourceDto> for RuleSource {
    fn from(value: RuleSourceDto) -> Self {
        match value {
            RuleSourceDto::Manual => Self::Manual,
            RuleSourceDto::Preset => Self::Preset,
        }
    }
}

/// Wire shape of [`AppDenyRule`].
///
/// Internally tagged on `type` so the Svelte form can discriminate
/// without resorting to instanceof checks. `SourceApp` rules carry
/// the typed identifier + a human-readable label for UI; `Pattern`
/// rules wrap the legacy free-text substring shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppDenyRuleDto {
    SourceApp {
        kind: SourceAppIdKindDto,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        source: RuleSourceDto,
    },
    Pattern {
        value: String,
    },
}

impl From<AppDenyRule> for AppDenyRuleDto {
    fn from(value: AppDenyRule) -> Self {
        match value {
            AppDenyRule::SourceApp {
                kind,
                value,
                label,
                source,
            } => Self::SourceApp {
                kind: kind.into(),
                value,
                label,
                source: source.into(),
            },
            AppDenyRule::Pattern { value } => Self::Pattern { value },
        }
    }
}

impl From<AppDenyRuleDto> for AppDenyRule {
    fn from(value: AppDenyRuleDto) -> Self {
        match value {
            AppDenyRuleDto::SourceApp {
                kind,
                value,
                label,
                source,
            } => Self::SourceApp {
                kind: kind.into(),
                value,
                label,
                source: source.into(),
            },
            AppDenyRuleDto::Pattern { value } => Self::Pattern { value },
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
    #[serde(default = "nagori_core::settings::default_max_image_entry_size_bytes")]
    pub max_image_entry_size_bytes: usize,
    #[serde(default = "default_capture_kind_dtos")]
    pub capture_kinds: Vec<ContentKindDto>,
    pub max_total_bytes: Option<u64>,
    pub capture_enabled: bool,
    pub auto_paste_enabled: bool,
    #[serde(default)]
    pub paste_format_default: PasteFormatDto,
    pub paste_delay_ms: u64,
    pub app_denylist: Vec<AppDenyRuleDto>,
    pub regex_denylist: Vec<String>,
    #[serde(default = "default_ai_settings_dto")]
    pub ai: AiSettingsDto,
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
    #[serde(default)]
    pub permanent_delete_on_delete: bool,
    #[serde(default)]
    pub block_sensitive_captures: bool,
    #[serde(default = "nagori_core::settings::default_capture_initial_clipboard_on_launch")]
    pub capture_initial_clipboard_on_launch: bool,
    #[serde(default = "nagori_core::settings::default_auto_update_check")]
    pub auto_update_check: bool,
    #[serde(default)]
    pub update_channel: UpdateChannelDto,
    #[serde(default = "nagori_core::settings::default_max_thumbnail_total_bytes")]
    pub max_thumbnail_total_bytes: Option<u64>,
    /// Onboarding lifecycle markers. `#[serde(default)]` keeps older settings
    /// snapshots forward-compatible — clients that predate the field simply
    /// omit it, which deserialises to all-`None`.
    #[serde(default)]
    pub onboarding: OnboardingSettingsDto,
    /// Optimistic-concurrency token, not a persisted setting. `get_settings`
    /// stamps the current value and `settings_changed` broadcasts carry it so
    /// the window can keep its baseline fresh; `update_settings` reads the
    /// caller's value as the compare-and-swap base. It is *not* part of the
    /// `AppSettings` <-> DTO body conversion (the core type carries no
    /// revision), so it defaults to 0 and is set by the command layer.
    #[serde(default)]
    pub revision: u64,
}

/// Default `ai` block for snapshots that predate the namespace.
fn default_ai_settings_dto() -> AiSettingsDto {
    AiSettings::default().into()
}

/// Wire shape of [`OnboardingSettings`]. Mirrors the camelCase field
/// names already used elsewhere in the DTO surface so the renderer never
/// sees the `snake_case` core form.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
// `accessibility_*_at` / `completed_at` are timestamps by nature; the
// "all-fields-end-in-at" lint is noisier than useful here.
#[allow(clippy::struct_field_names)]
pub struct OnboardingSettingsDto {
    #[serde(with = "time::serde::rfc3339::option")]
    pub accessibility_prompted_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub accessibility_first_granted_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
}

impl From<OnboardingSettings> for OnboardingSettingsDto {
    fn from(value: OnboardingSettings) -> Self {
        Self {
            accessibility_prompted_at: value.accessibility_prompted_at,
            accessibility_first_granted_at: value.accessibility_first_granted_at,
            completed_at: value.completed_at,
        }
    }
}

impl From<OnboardingSettingsDto> for OnboardingSettings {
    fn from(value: OnboardingSettingsDto) -> Self {
        Self {
            accessibility_prompted_at: value.accessibility_prompted_at,
            accessibility_first_granted_at: value.accessibility_first_granted_at,
            completed_at: value.completed_at,
        }
    }
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
            max_image_entry_size_bytes: value.max_image_entry_size_bytes,
            capture_kinds: value.capture_kinds.into_iter().map(Into::into).collect(),
            max_total_bytes: value.max_total_bytes,
            capture_enabled: value.capture_enabled,
            auto_paste_enabled: value.auto_paste_enabled,
            paste_format_default: value.paste_format_default.into(),
            paste_delay_ms: value.paste_delay_ms,
            app_denylist: value.app_denylist.into_iter().map(Into::into).collect(),
            regex_denylist: value.regex_denylist,
            ai: value.ai.into(),
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
            permanent_delete_on_delete: value.permanent_delete_on_delete,
            block_sensitive_captures: value.block_sensitive_captures,
            capture_initial_clipboard_on_launch: value.capture_initial_clipboard_on_launch,
            auto_update_check: value.auto_update_check,
            update_channel: value.update_channel.into(),
            max_thumbnail_total_bytes: value.max_thumbnail_total_bytes,
            onboarding: value.onboarding.into(),
            // Revision is storage metadata, not part of the core settings body.
            // The command / broadcast layer overwrites this with the live token
            // after the conversion; 0 is the correct default for an unstamped
            // DTO, and `From<AppSettingsDto>` drops it (the core type has none).
            revision: 0,
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
            max_image_entry_size_bytes: value.max_image_entry_size_bytes,
            capture_kinds: value.capture_kinds.into_iter().map(Into::into).collect(),
            max_total_bytes: value.max_total_bytes,
            capture_enabled: value.capture_enabled,
            auto_paste_enabled: value.auto_paste_enabled,
            paste_format_default: value.paste_format_default.into(),
            paste_delay_ms: value.paste_delay_ms,
            app_denylist: value.app_denylist.into_iter().map(Into::into).collect(),
            regex_denylist: value.regex_denylist,
            ai: value.ai.into(),
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
            permanent_delete_on_delete: value.permanent_delete_on_delete,
            block_sensitive_captures: value.block_sensitive_captures,
            capture_initial_clipboard_on_launch: value.capture_initial_clipboard_on_launch,
            auto_update_check: value.auto_update_check,
            update_channel: value.update_channel.into(),
            max_thumbnail_total_bytes: value.max_thumbnail_total_bytes,
            onboarding: value.onboarding.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use nagori_core::{
        AppDenyRule, AppSettings, Appearance, ContentKind, PasteFormat, RecentOrder, RuleSource,
        SecretHandling, SourceAppIdKind, UpdateChannel,
    };
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    #[test]
    #[allow(clippy::too_many_lines)]
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
            max_image_entry_size_bytes: 32 * 1024 * 1024,
            capture_kinds: [ContentKind::Text, ContentKind::Image]
                .into_iter()
                .collect(),
            max_total_bytes: Some(64 * 1024 * 1024),
            capture_enabled: false,
            auto_paste_enabled: true,
            paste_format_default: PasteFormat::PlainText,
            paste_delay_ms: 80,
            app_denylist: vec![
                AppDenyRule::SourceApp {
                    kind: SourceAppIdKind::MacosBundleId,
                    value: "com.1password.1password".to_owned(),
                    label: Some("1Password".to_owned()),
                    source: RuleSource::Preset,
                },
                AppDenyRule::Pattern {
                    value: "Bitwarden".to_owned(),
                },
            ],
            regex_denylist: vec!["INTERNAL-\\d+".to_owned()],
            ai: AiSettings {
                enabled: true,
                provider: AiProviderKind::AppleNative,
                allowed_actions: vec![AiActionId::Summarize],
                allow_streaming: false,
                request_timeout_ms: 12_345,
                semantic_index_enabled: true,
                semantic_index_ac_power_only: false,
                onboarding_dismissed: true,
                allow_openai_fallback_prompt: false,
            },
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
            permanent_delete_on_delete: true,
            block_sensitive_captures: true,
            capture_initial_clipboard_on_launch: false,
            auto_update_check: false,
            update_channel: UpdateChannel::Stable,
            max_thumbnail_total_bytes: Some(32 * 1024 * 1024),
            onboarding: nagori_core::settings::OnboardingSettings {
                accessibility_prompted_at: Some(OffsetDateTime::UNIX_EPOCH),
                accessibility_first_granted_at: None,
                completed_at: None,
            },
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
        assert_eq!(
            restored.max_image_entry_size_bytes,
            original.max_image_entry_size_bytes
        );
        assert_eq!(restored.capture_kinds, original.capture_kinds);
        assert_eq!(restored.max_total_bytes, original.max_total_bytes);
        assert_eq!(restored.capture_enabled, original.capture_enabled);
        assert_eq!(restored.auto_paste_enabled, original.auto_paste_enabled);
        assert_eq!(restored.paste_format_default, original.paste_format_default);
        assert_eq!(restored.paste_delay_ms, original.paste_delay_ms);
        assert_eq!(restored.app_denylist, original.app_denylist);
        assert_eq!(restored.regex_denylist, original.regex_denylist);
        assert_eq!(restored.ai, original.ai);
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
        assert!(restored.permanent_delete_on_delete);
        assert!(restored.block_sensitive_captures);
        assert!(!restored.capture_initial_clipboard_on_launch);
        assert!(!restored.auto_update_check);
        assert!(matches!(restored.update_channel, UpdateChannel::Stable));
    }

    #[test]
    fn onboarding_dto_serialises_as_camel_case_rfc3339() {
        // The frontend reads `onboarding.accessibilityPromptedAt` etc.
        // as RFC3339 strings (or `null`). Pin both the camelCase rename
        // and the RFC3339 serialisation so a future serde tweak on the
        // `time::serde::rfc3339::option` adapter cannot silently break
        // the wire format. Also asserts the absent marker emits `null`
        // rather than being skipped — the TS contract treats absence as
        // a JSON parsing error.
        let stamped =
            OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("static timestamp parses");
        let core = nagori_core::OnboardingSettings {
            accessibility_prompted_at: Some(stamped),
            accessibility_first_granted_at: None,
            completed_at: None,
        };
        let dto: OnboardingSettingsDto = core.clone().into();
        let json = serde_json::to_value(&dto).expect("serialise");
        assert_eq!(
            json["accessibilityPromptedAt"],
            json!("2023-11-14T22:13:20Z")
        );
        assert_eq!(json["accessibilityFirstGrantedAt"], json!(null));
        assert_eq!(json["completedAt"], json!(null));
        // snake_case must not coexist with camelCase rename.
        assert!(
            json.get("accessibility_prompted_at").is_none() && json.get("completed_at").is_none(),
            "snake_case fields must not appear on the wire",
        );
        // Round-trip the JSON back through the DTO and into the core
        // type so the timestamp survives the conversion.
        let parsed: OnboardingSettingsDto =
            serde_json::from_value(json).expect("deserialise OnboardingSettingsDto");
        let restored: nagori_core::OnboardingSettings = parsed.into();
        assert_eq!(restored, core);
    }

    #[test]
    fn app_settings_dto_serializes_secret_handling_as_snake_case() {
        // The Tauri command boundary speaks JSON — the Svelte side reads
        // `secret_handling: "store_redacted"`, so the snake_case rename must
        // survive any future churn on the enum.
        let dto: AppSettingsDto = AppSettings::default().into();
        let json = serde_json::to_value(&dto).expect("serialize");
        assert_eq!(json["secretHandling"], json!("store_redacted"));
        assert_eq!(json["ai"]["provider"], json!("disabled"));
        assert_eq!(json["ai"]["enabled"], json!(false));
        assert_eq!(json["locale"], json!("system"));
        assert_eq!(json["pasteFormatDefault"], json!("preserve"));
        assert_eq!(json["recentOrder"], json!("by_recency"));
        assert_eq!(json["appearance"], json!("system"));
    }

    #[test]
    fn app_deny_rule_dto_wire_shape_is_stable() {
        // Pin the JSON layout the Svelte side reads. Source-app rules
        // surface their kind / value / label / source verbatim;
        // pattern rules drop the typed fields entirely. A future
        // refactor that renames `type` to `kind` or adds an enum
        // variant without a snake_case rename would silently break
        // the form.
        let source_rule = AppDenyRuleDto::SourceApp {
            kind: SourceAppIdKindDto::MacosBundleId,
            value: "com.example.app".to_owned(),
            label: Some("Example".to_owned()),
            source: RuleSourceDto::Preset,
        };
        let source_json = serde_json::to_value(&source_rule).expect("serialize");
        assert_eq!(
            source_json,
            json!({
                "type": "source_app",
                "kind": "macos_bundle_id",
                "value": "com.example.app",
                "label": "Example",
                "source": "preset",
            }),
        );

        let pattern_rule = AppDenyRuleDto::Pattern {
            value: "MyApp".to_owned(),
        };
        let pattern_json = serde_json::to_value(&pattern_rule).expect("serialize");
        assert_eq!(pattern_json, json!({ "type": "pattern", "value": "MyApp" }),);
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
}

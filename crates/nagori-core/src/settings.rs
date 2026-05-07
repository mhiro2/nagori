use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ContentKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub global_hotkey: String,
    pub history_retention_count: usize,
    pub history_retention_days: Option<u32>,
    pub max_entry_size_bytes: usize,
    #[serde(default = "default_capture_kinds")]
    pub capture_kinds: BTreeSet<ContentKind>,
    pub max_total_bytes: Option<u64>,
    pub capture_enabled: bool,
    pub auto_paste_enabled: bool,
    pub paste_format_default: PasteFormat,
    pub paste_delay_ms: u64,
    pub app_denylist: Vec<String>,
    pub regex_denylist: Vec<String>,
    pub local_only_mode: bool,
    pub ai_provider: AiProviderSetting,
    pub ai_enabled: bool,
    pub semantic_search_enabled: bool,
    pub cli_ipc_enabled: bool,
    pub locale: Locale,
    pub recent_order: RecentOrder,
    pub appearance: Appearance,
    /// macOS launch-at-login. Surfaced through `tauri-plugin-autostart`
    /// when the desktop app starts. Defaults to `false` so existing
    /// installations stay opt-in.
    pub auto_launch: bool,
    /// What the capture pipeline does when an entry classifies as
    /// `Sensitivity::Secret` (api keys, JWTs, private keys, etc). Defaults to
    /// `StoreRedacted` so the durable copy on disk is the redacted form, not
    /// the raw secret — even if the user later exports the DB.
    pub secret_handling: SecretHandling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiProviderSetting {
    None,
    Local,
    Remote { name: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteFormat {
    #[default]
    Preserve,
    PlainText,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecentOrder {
    #[default]
    ByRecency,
    ByUseCount,
    PinnedFirstThenRecency,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    Light,
    Dark,
    #[default]
    System,
}

/// Handling strategy for entries classified as `Sensitivity::Secret`.
///
/// The capture loop and `nagori add` consult this when a Secret-tagged
/// entry would otherwise land in storage. The default `StoreRedacted` is
/// chosen so that an exported / leaked database never contains raw secret
/// material, even at the cost of being unable to re-paste the original.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretHandling {
    /// Refuse to store the entry at all. Equivalent to a `Blocked` outcome
    /// but driven by classification, not source-app denylist.
    Block,
    /// Persist the redacted form (built-in patterns + user regexes) as the
    /// authoritative content. Default — the most user-respecting option that
    /// still keeps disk storage safe.
    #[default]
    StoreRedacted,
    /// Persist the original text. Preview is still redacted in the UI, but
    /// the underlying entry retains the raw secret so the user can copy it
    /// back later. Opt-in only.
    StoreFull,
}

/// User-facing language for the desktop UI. Backend log/audit messages and
/// the CLI surface are English-only; this only affects the `WebView` strings.
///
/// Wire format uses BCP-47-ish tags: `en`, `ja`, `ko`, `zh-Hans`. The casing of
/// `zh-Hans` is preserved because it is the canonical script subtag and the
/// frontend negotiation maps any `zh-*` regional preference onto it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "ja")]
    Ja,
    #[serde(rename = "ko")]
    Ko,
    #[serde(rename = "zh-Hans")]
    ZhHans,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            global_hotkey: "CmdOrCtrl+Shift+V".to_owned(),
            history_retention_count: 10_000,
            history_retention_days: Some(90),
            max_entry_size_bytes: 512 * 1024,
            capture_kinds: default_capture_kinds(),
            max_total_bytes: None,
            capture_enabled: true,
            auto_paste_enabled: true,
            paste_format_default: PasteFormat::default(),
            paste_delay_ms: 60,
            app_denylist: vec![
                "1Password".to_owned(),
                "Bitwarden".to_owned(),
                "KeePassXC".to_owned(),
                "Apple Passwords".to_owned(),
            ],
            regex_denylist: Vec::new(),
            local_only_mode: true,
            ai_provider: AiProviderSetting::None,
            ai_enabled: false,
            semantic_search_enabled: false,
            cli_ipc_enabled: true,
            locale: Locale::default(),
            recent_order: RecentOrder::default(),
            appearance: Appearance::default(),
            auto_launch: false,
            secret_handling: SecretHandling::default(),
        }
    }
}

pub fn default_capture_kinds() -> BTreeSet<ContentKind> {
    [
        ContentKind::Text,
        ContentKind::Url,
        ContentKind::Code,
        ContentKind::Image,
        ContentKind::FileList,
        ContentKind::RichText,
        ContentKind::Unknown,
    ]
    .into_iter()
    .collect()
}

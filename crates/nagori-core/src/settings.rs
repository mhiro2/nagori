use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ContentKind;
use crate::errors::{AppError, Result};
use crate::limits::MAX_ENTRY_SIZE_BYTES;

/// Maximum entries the user can ask retention to keep. Beyond this the
/// retention sweep would no longer fit in a single transaction without
/// risking commit timeouts on slower disks.
pub const MAX_RETENTION_COUNT: usize = 1_000_000;

/// Upper bound for `history_retention_days`. ~10 years of capture; values
/// above this stop being meaningful for a clipboard manager and start
/// hurting retention sweep performance.
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// Upper bound for `paste_delay_ms`.
///
/// The synthesised ⌘V keystroke needs a few-tens-of-ms wait after focus
/// restoration, but anything beyond a second is indistinguishable from
/// "paste hung" to the user — and at `u64::MAX` the palette would deadlock
/// until the OS killed the daemon.
pub const MAX_PASTE_DELAY_MS: u64 = 1000;

/// Visible row range for the palette result list. Below 1 the palette is
/// empty; above 64 layout overflows the popup and wastes the LRU.
pub const MAX_PALETTE_ROW_COUNT: u32 = 64;

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
    /// Launch Nagori automatically when the user signs in. Surfaced
    /// through `tauri-plugin-autostart`, which writes a macOS
    /// `LaunchAgent` plist, a Windows `HKCU\…\Run` registry entry, or a
    /// Linux `~/.config/autostart/<bundle>.desktop` file depending on
    /// the OS. Defaults to `false` so existing installations stay
    /// opt-in.
    pub auto_launch: bool,
    /// What the capture pipeline does when an entry classifies as
    /// `Sensitivity::Secret` (api keys, JWTs, private keys, etc). Defaults to
    /// `StoreRedacted` so the durable copy on disk is the redacted form, not
    /// the raw secret — even if the user later exports the DB.
    pub secret_handling: SecretHandling,
    /// User-overridable bindings for in-palette local actions. Missing keys
    /// fall back to the built-in defaults defined on the frontend; this map
    /// is intentionally sparse so users only need to record overrides.
    #[serde(default)]
    pub palette_hotkeys: BTreeMap<PaletteHotkeyAction, String>,
    /// Optional auxiliary global shortcuts beyond `global_hotkey`. The value
    /// is the same accelerator-string format `tauri-plugin-global-shortcut`
    /// accepts. Empty entries are ignored.
    #[serde(default)]
    pub secondary_hotkeys: BTreeMap<SecondaryHotkeyAction, String>,
    /// Number of result rows displayed in the palette before scrolling kicks
    /// in. Used purely for visual sizing — search itself is independent.
    #[serde(default = "default_palette_row_count")]
    pub palette_row_count: u32,
    /// Whether the right-hand preview pane is shown. When `false` the
    /// palette becomes single-column for higher information density.
    #[serde(default = "default_show_preview_pane")]
    pub show_preview_pane: bool,
    /// Whether the system tray icon is visible — the macOS menu bar
    /// entry, the Windows notification-area icon, or the Linux
    /// `StatusNotifierItem` / app-indicator. When `false` the user
    /// reaches Nagori only through the global hotkey / CLI. The field
    /// name predates Windows / Linux support and is kept for settings
    /// persistence compatibility.
    #[serde(default = "default_show_in_menu_bar")]
    pub show_in_menu_bar: bool,
    /// When `true`, all non-pinned entries are cleared during app shutdown.
    /// Pinned entries are always preserved.
    #[serde(default)]
    pub clear_on_quit: bool,
    /// When `false`, the capture loop discards the very first clipboard
    /// sequence it sees on launch (skipping whatever was already on the
    /// pasteboard before Nagori started). Default `true` preserves the
    /// previous behaviour of capturing the existing clipboard contents.
    #[serde(default = "default_capture_initial_clipboard_on_launch")]
    pub capture_initial_clipboard_on_launch: bool,
    /// Whether the desktop shell probes the updater endpoint at startup to
    /// surface a "new version available" notification. Disabling this leaves
    /// the manual "Check for updates now" button as the only path.
    #[serde(default = "default_auto_update_check")]
    pub auto_update_check: bool,
    /// Release channel the updater JSON is fetched from. Only `stable` is
    /// currently shipped; the field is persisted so future channels can be
    /// added without breaking the on-disk shape.
    #[serde(default)]
    pub update_channel: UpdateChannel,
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

/// Updater release channel.
///
/// Persisted in `AppSettings.update_channel` so the frontend can show the
/// active channel and so tests pin the wire format — the only variant
/// today is `Stable`, but keeping this an enum lets future `Beta` /
/// `Nightly` rollouts land without a settings migration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    #[default]
    Stable,
}

impl UpdateChannel {
    /// Stable wire-format token, used by the CLI doctor output and the
    /// frontend label rendering. Matches the serde rename above.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
        }
    }
}

/// Identifier for a user-rebindable in-palette action.
///
/// The frontend owns the default key bindings; this enum exists so the
/// override map has a stable wire format that does not drift if the UI
/// introduces alias actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaletteHotkeyAction {
    Pin,
    Delete,
    PasteAsPlain,
    CopyWithoutPaste,
    Clear,
    OpenPreview,
}

/// Identifier for an auxiliary global shortcut.
///
/// Each variant is registered alongside the primary palette hotkey
/// independently, and may be left unbound by omitting the entry from the
/// settings map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecondaryHotkeyAction {
    /// Re-paste the most recently used entry without opening the palette.
    RepasteLast,
    /// Clear non-pinned history.
    ClearHistory,
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
/// Wire format uses BCP-47-ish tags plus the special sentinel `system`. The
/// casing of `zh-Hans` / `zh-Hant` is preserved because the script subtag is
/// the canonical disambiguator for Simplified vs. Traditional Chinese, and
/// the frontend negotiation routes any `zh-*` regional preference onto one of
/// those two based on the `Hant` script.
///
/// `System` is the default. It is the persisted *preference*, not a
/// dictionary key — the frontend resolves it to a concrete locale on each
/// load by reading the OS / `WebView` language preferences, so changing the
/// OS language follows through to the UI without touching settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Locale {
    #[default]
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

impl AppSettings {
    /// Validate value-range invariants that the wire format alone cannot
    /// enforce. Run on every entry point that mutates persisted settings —
    /// the storage `save_settings` path, the IPC `UpdateSettings` handler,
    /// and the desktop / CLI startup load — so a hand-edited config file or
    /// a buggy frontend cannot wedge the daemon with values like
    /// `paste_delay_ms = u64::MAX` or `palette_row_count = 0`.
    pub fn validate(&self) -> Result<()> {
        validate_hotkey(&self.global_hotkey)?;
        if !(1..=MAX_RETENTION_COUNT).contains(&self.history_retention_count) {
            return Err(AppError::InvalidInput(format!(
                "history_retention_count must be between 1 and {MAX_RETENTION_COUNT}"
            )));
        }
        if !(1..=MAX_ENTRY_SIZE_BYTES).contains(&self.max_entry_size_bytes) {
            return Err(AppError::InvalidInput(format!(
                "max_entry_size_bytes must be between 1 and {MAX_ENTRY_SIZE_BYTES}"
            )));
        }
        if let Some(days) = self.history_retention_days
            && (days == 0 || days > MAX_RETENTION_DAYS)
        {
            return Err(AppError::InvalidInput(format!(
                "history_retention_days must be between 1 and {MAX_RETENTION_DAYS}"
            )));
        }
        if self.paste_delay_ms > MAX_PASTE_DELAY_MS {
            return Err(AppError::InvalidInput(format!(
                "paste_delay_ms must be <= {MAX_PASTE_DELAY_MS}"
            )));
        }
        if !(1..=MAX_PALETTE_ROW_COUNT).contains(&self.palette_row_count) {
            return Err(AppError::InvalidInput(format!(
                "palette_row_count must be between 1 and {MAX_PALETTE_ROW_COUNT}"
            )));
        }
        Ok(())
    }
}

/// Validate a Tauri-style global accelerator string ahead of persistence.
///
/// Tauri global-shortcut format: zero or more modifiers, `+`-separated, then
/// exactly one key segment. We can't fully verify the OS will accept the
/// final binding (that depends on the Tauri parser at register time), but
/// catching the obvious shape mistakes here means a typo'd hotkey from the
/// settings UI never lands in storage and silently disables the feature
/// after the next restart.
pub fn validate_hotkey(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(
            "global_hotkey must not be empty".to_owned(),
        ));
    }
    if trimmed != raw {
        return Err(AppError::InvalidInput(
            "global_hotkey must not have leading/trailing whitespace".to_owned(),
        ));
    }
    let segments: Vec<&str> = trimmed.split('+').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(AppError::InvalidInput(
            "global_hotkey must not contain empty `+` segments".to_owned(),
        ));
    }
    let (key, mods) = segments.split_last().expect("non-empty after trim check");
    let mut seen = std::collections::HashSet::new();
    for m in mods {
        let canonical = canonical_modifier(m).ok_or_else(|| {
            AppError::InvalidInput(format!("global_hotkey: unknown modifier `{m}`"))
        })?;
        if !seen.insert(canonical) {
            return Err(AppError::InvalidInput(format!(
                "global_hotkey: duplicate modifier `{m}`"
            )));
        }
    }
    if canonical_modifier(key).is_some() {
        return Err(AppError::InvalidInput(
            "global_hotkey must end with a non-modifier key".to_owned(),
        ));
    }
    if !is_valid_hotkey_key(key) {
        return Err(AppError::InvalidInput(format!(
            "global_hotkey: invalid key `{key}`"
        )));
    }
    Ok(())
}

fn canonical_modifier(token: &str) -> Option<&'static str> {
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" | "meta" | "win" | "windows" => Some("super"),
        "ctrl" | "control" => Some("ctrl"),
        "cmdorctrl" | "commandorcontrol" => Some("cmdorctrl"),
        "alt" | "option" | "opt" => Some("alt"),
        "shift" => Some("shift"),
        _ => None,
    }
}

fn is_valid_hotkey_key(key: &str) -> bool {
    // Single printable ASCII char (letter/digit/punct), or a named key from
    // the known whitelist. This mirrors what `tauri-plugin-global-shortcut`
    // accepts on macOS today; new tokens can be added here as needed.
    if key.chars().count() == 1 {
        let c = key.chars().next().expect("len-checked above");
        return c.is_ascii_alphanumeric() || "`-=[]\\;',./".contains(c);
    }
    let upper = key.to_ascii_uppercase();
    if upper.starts_with('F')
        && upper.len() <= 3
        && upper[1..].chars().all(|c| c.is_ascii_digit())
        && let Ok(n) = upper[1..].parse::<u32>()
    {
        return (1..=24).contains(&n);
    }
    matches!(
        upper.as_str(),
        "SPACE"
            | "ENTER"
            | "RETURN"
            | "ESC"
            | "ESCAPE"
            | "TAB"
            | "BACKSPACE"
            | "DELETE"
            | "INSERT"
            | "UP"
            | "DOWN"
            | "LEFT"
            | "RIGHT"
            | "HOME"
            | "END"
            | "PAGEUP"
            | "PAGEDOWN"
            | "CAPSLOCK"
            | "NUMLOCK"
            | "SCROLLLOCK"
            | "PRINTSCREEN"
    )
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
            palette_hotkeys: BTreeMap::new(),
            secondary_hotkeys: BTreeMap::new(),
            palette_row_count: default_palette_row_count(),
            show_preview_pane: default_show_preview_pane(),
            show_in_menu_bar: default_show_in_menu_bar(),
            clear_on_quit: false,
            capture_initial_clipboard_on_launch: default_capture_initial_clipboard_on_launch(),
            auto_update_check: default_auto_update_check(),
            update_channel: UpdateChannel::default(),
        }
    }
}

pub const fn default_palette_row_count() -> u32 {
    8
}

pub const fn default_show_preview_pane() -> bool {
    true
}

pub const fn default_show_in_menu_bar() -> bool {
    true
}

pub const fn default_capture_initial_clipboard_on_launch() -> bool {
    true
}

pub const fn default_auto_update_check() -> bool {
    true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_hotkey_accepts_common_shapes() {
        for ok in [
            "CmdOrCtrl+Shift+V",
            "Cmd+V",
            "Ctrl+Alt+P",
            "Shift+F12",
            "Alt+Space",
            "CmdOrCtrl+Enter",
        ] {
            validate_hotkey(ok).unwrap_or_else(|err| panic!("expected `{ok}` to validate: {err}"));
        }
    }

    #[test]
    fn validate_hotkey_rejects_bad_shapes() {
        for bad in [
            "",
            "  ",
            "Cmd",               // modifier only
            "Cmd+",              // empty key
            "+Cmd+V",            // empty leading segment
            "Cmd++V",            // empty middle segment
            "Cmd+Foo+V",         // unknown modifier
            "Cmd+Shift+Shift+V", // duplicate modifier (after canonicalization)
            "Cmd+F25",           // function key out of range
            "Cmd+Hyperspace",    // unknown named key
            " Cmd+V",            // leading whitespace
            "Cmd+V ",            // trailing whitespace
        ] {
            assert!(
                validate_hotkey(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }
}

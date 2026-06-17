use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;

use crate::ContentKind;
use crate::errors::{AppError, Result};
use crate::limits::MAX_ENTRY_SIZE_BYTES;
use crate::model::{AiActionId, AiProviderKind};
use crate::policy::compile_user_regex;

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

/// Upper bound for `ai.request_timeout_ms`.
///
/// A zero timeout collapses every AI request into an instant deadline (the
/// `Duration::from_millis(0)` the daemon hands `guard_event_stream`), so it
/// is rejected separately. Ten minutes is far longer than any on-device or
/// remote action should need; beyond it a wedged provider could pin a
/// concurrency permit for an unreasonable stretch.
pub const MAX_AI_REQUEST_TIMEOUT_MS: u64 = 600_000;

/// Maximum number of `regex_denylist` patterns the user can configure.
///
/// Each pattern is run against every captured clip, so the per-pattern `DoS`
/// limits (length / nesting / NFA + DFA size) bound a single rule but not the
/// aggregate: thousands of rules would turn each capture into thousands of
/// regex executions. 128 is far above any realistic redaction rule set while
/// keeping the per-capture classifier cost bounded.
pub const MAX_USER_REGEX_COUNT: usize = 128;

/// Identifier kind for a [`AppDenyRule::SourceApp`] entry.
///
/// Each variant pins which `SourceApp` field the matcher should compare
/// against. The split exists so a macOS bundle ID, a Windows executable
/// name, and a Linux desktop ID can sit side-by-side on the same
/// denylist without sharing an opaque string column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAppIdKind {
    /// macOS bundle identifier (`com.example.app`).
    MacosBundleId,
    /// Basename of the Windows executable, without the `.exe` suffix.
    WindowsExeName,
    /// Fully-qualified Windows executable path. Compared with separator
    /// + case normalisation.
    WindowsExecutablePath,
    /// Linux freedesktop application ID (matches `.desktop` file basename).
    LinuxDesktopId,
    /// Linux Flatpak application ID (`org.example.App`).
    LinuxFlatpakId,
    /// X11 `WM_CLASS` value (reserved for the future X11 platform path).
    X11WmClass,
}

/// Provenance of an [`AppDenyRule`].
///
/// Distinguishes rules that the user explicitly added (`Manual`) from
/// rules that came from a curated preset (`Preset`). The UI uses this
/// to decide whether a row is editable, and the audit story benefits
/// from being able to point at the preset that introduced a rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    #[default]
    Manual,
    Preset,
}

/// A single entry on the source-app denylist.
///
/// `SourceApp` rules carry a typed identifier (e.g. a macOS bundle ID)
/// and are matched by exact value against the corresponding `SourceApp`
/// field — drift-free in the common "block 1Password" case.
/// `Pattern` rules preserve the original free-text substring matching
/// behaviour so existing settings keep working and advanced users have
/// a hatch for cases the typed identifiers do not cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppDenyRule {
    /// Typed identifier match. Carries the kind, the identifier value,
    /// an optional human-readable label for UI, and the rule's
    /// provenance.
    SourceApp {
        kind: SourceAppIdKind,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default)]
        source: RuleSource,
    },
    /// Free-text substring match against
    /// `name + bundle_id + executable_path`. Case-insensitive.
    Pattern { value: String },
}

/// One entry in a bundled preset dictionary. Static, owned by the
/// crate, expanded into [`AppDenyRule::SourceApp`] entries on demand.
#[derive(Debug, Clone, Copy)]
pub struct PresetEntry {
    pub kind: SourceAppIdKind,
    pub value: &'static str,
    pub label: &'static str,
}

impl PresetEntry {
    /// Materialise this preset entry as a concrete [`AppDenyRule`].
    /// `source` is stamped `Preset` so the settings UI can render the
    /// row read-only and the audit log keeps track of where the rule
    /// originated.
    #[must_use]
    pub fn to_rule(self) -> AppDenyRule {
        AppDenyRule::SourceApp {
            kind: self.kind,
            value: self.value.to_owned(),
            label: Some(self.label.to_owned()),
            source: RuleSource::Preset,
        }
    }
}

/// Curated password-manager preset. Covers the apps the project's
/// privacy guarantee names explicitly, with both macOS bundle IDs and
/// Windows executable names so the rule fires on either platform.
///
/// Kept hardcoded (not loaded over the network) so an offline install,
/// a hostile DNS, or a supply-chain compromise of an update channel
/// cannot widen or narrow the policy without an app release.
pub const PASSWORD_MANAGER_PRESET: &[PresetEntry] = &[
    // 1Password — modern and Setapp builds share the v8 bundle ID, v7
    // ships under its own. Listing both rules out the "I'm on the v7
    // build" miss.
    PresetEntry {
        kind: SourceAppIdKind::MacosBundleId,
        value: "com.1password.1password",
        label: "1Password",
    },
    PresetEntry {
        kind: SourceAppIdKind::MacosBundleId,
        value: "com.agilebits.onepassword7",
        label: "1Password 7",
    },
    PresetEntry {
        kind: SourceAppIdKind::MacosBundleId,
        value: "com.agilebits.onepassword4",
        label: "1Password (legacy)",
    },
    // Bitwarden desktop. The browser extensions live in the browser
    // process, so the host browser's bundle ID would shadow them —
    // the desktop client is the meaningful target.
    PresetEntry {
        kind: SourceAppIdKind::MacosBundleId,
        value: "com.bitwarden.desktop",
        label: "Bitwarden",
    },
    PresetEntry {
        kind: SourceAppIdKind::MacosBundleId,
        value: "org.keepassxc.keepassxc",
        label: "KeePassXC",
    },
    PresetEntry {
        kind: SourceAppIdKind::MacosBundleId,
        value: "com.apple.Passwords",
        label: "Apple Passwords",
    },
    // Windows side: compare the executable basename (no `.exe`),
    // case-insensitively. The exe name is the most stable identifier
    // we can pin without bringing in MSIX / Program Files (x86) path
    // normalisation.
    PresetEntry {
        kind: SourceAppIdKind::WindowsExeName,
        value: "1password",
        label: "1Password (Windows)",
    },
    PresetEntry {
        kind: SourceAppIdKind::WindowsExeName,
        value: "bitwarden",
        label: "Bitwarden (Windows)",
    },
    PresetEntry {
        kind: SourceAppIdKind::WindowsExeName,
        value: "keepassxc",
        label: "KeePassXC (Windows)",
    },
];

/// Expand [`PASSWORD_MANAGER_PRESET`] into concrete [`AppDenyRule`]s.
///
/// Each rule is stamped with `RuleSource::Preset` so the Settings UI
/// can tell preset-managed entries apart from user-typed patterns.
/// Default settings call this so fresh installs ship with canonical
/// bundle IDs instead of the old hand-typed display-name strings
/// that may or may not have matched the real `SourceApp::name`.
#[must_use]
pub fn password_manager_preset_rules() -> Vec<AppDenyRule> {
    PASSWORD_MANAGER_PRESET
        .iter()
        .copied()
        .map(PresetEntry::to_rule)
        .collect()
}

/// Custom deserializer for `app_denylist` that accepts both the new
/// shape (`Vec<AppDenyRule>`) and the legacy free-text form
/// (`Vec<String>`).
///
/// A legacy settings snapshot stored each entry as a bare string;
/// the schema is now an internally-tagged enum. The settings JSON
/// blob lives in `SQLite`, so a hard format break would silently lose
/// the user's rules on first launch. Reading either shape — and
/// mapping the legacy strings to [`AppDenyRule::Pattern`] — keeps
/// every existing rule active across the upgrade.
///
/// Parsing is per-element so a single unreadable rule cannot abort the
/// whole [`AppSettings`] deserialize. A future `AppDenyRule` variant seen
/// by an older build (downgrade) or a hand-edited DB row would otherwise
/// fail the entire settings read — and a caller that falls back to
/// `AppSettings::default()` on that error would drop *every* denylist
/// rule, a fail-open privacy regression. Each element is buffered as an
/// untyped value first, then converted; the ones that do not parse are
/// warned about and skipped, so every still-readable rule survives.
pub fn deserialize_app_denylist<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AppDenyRule>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RuleOrString {
        Rule(AppDenyRule),
        Legacy(String),
    }
    let raw: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let mut rules = Vec::with_capacity(raw.len());
    for value in raw {
        match serde_json::from_value::<RuleOrString>(value) {
            Ok(RuleOrString::Rule(rule)) => rules.push(rule),
            Ok(RuleOrString::Legacy(value)) => rules.push(AppDenyRule::Pattern { value }),
            Err(err) => {
                tracing::warn!(error = %err, "app_denylist_rule_skipped");
            }
        }
    }
    Ok(rules)
}

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
    /// Cap on the aggregate `entry_thumbnails.byte_count`. `None` disables
    /// the LRU eviction sweep entirely; `Some(0)` evicts every thumbnail
    /// after each generation. The default budget (64 MiB) sits comfortably
    /// below the per-image cap of 256 KiB × ~256 entries, so a typical
    /// history can hold roughly a screen's worth of recent imagery cached.
    /// Thumbnails are derived from the original payload and regenerable on
    /// demand, so the budget can be cleared without losing user data.
    #[serde(default = "default_max_thumbnail_total_bytes")]
    pub max_thumbnail_total_bytes: Option<u64>,
    pub capture_enabled: bool,
    pub auto_paste_enabled: bool,
    pub paste_format_default: PasteFormat,
    pub paste_delay_ms: u64,
    /// Source-app denylist. Mixes typed identifier rules (preferred
    /// for the bundled "Password managers" preset) with free-text
    /// pattern rules (free-text substring match, retained for
    /// backward compatibility and for cases the typed identifiers do
    /// not cover). Deserialised through a custom path so a legacy
    /// snapshot persisted as `Vec<String>` is read as
    /// [`AppDenyRule::Pattern`] without losing the user's rules.
    // `default` resolves to `password_manager_preset_rules` rather than
    // `Vec::default`. Field-level `#[serde(default)]` would call
    // `<Vec<AppDenyRule>>::default()` (empty), but the struct-level
    // intent for a missing field is "user has not opted out of the
    // preset" — i.e. fall back to the same preset the struct's
    // `Default::default()` ships with. Without this, a pre-1.0 settings
    // row that omitted the field would silently drop the password-
    // manager preset on read.
    #[serde(
        deserialize_with = "deserialize_app_denylist",
        default = "password_manager_preset_rules"
    )]
    pub app_denylist: Vec<AppDenyRule>,
    pub regex_denylist: Vec<String>,
    /// AI feature configuration. Replaces the former flat `ai_enabled` /
    /// `ai_provider` / `semantic_search_enabled` triple. Older settings rows
    /// that still carry those keys deserialize cleanly because unknown fields
    /// are ignored and the new `ai` namespace falls back to its default
    /// (`enabled = false`), matching the previous "AI off" behaviour.
    #[serde(default)]
    pub ai: AiSettings,
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
    /// When `true`, per-entry delete performs an immediate hard delete instead
    /// of writing a tombstone for the maintenance loop to reclaim later.
    #[serde(default)]
    pub permanent_delete_on_delete: bool,
    /// When `true`, captures classified as `Private` or `Secret` are refused
    /// storage entirely. This is stricter than `secret_handling=block` because
    /// it also covers the Private tier.
    #[serde(default)]
    pub block_sensitive_captures: bool,
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
    /// Onboarding lifecycle markers. Drives the Setup tab's "have we ever
    /// asked the user for Accessibility?" / "have we ever observed a
    /// grant?" decisions so the UI can distinguish a real first-launch
    /// from a previously-granted-then-revoked install.
    #[serde(default)]
    pub onboarding: OnboardingSettings,
}

/// Persisted onboarding state used to derive the Setup card's UI state.
///
/// All fields are optional so a fresh install starts with `None`s and the
/// frontend's decision tree collapses to `NotRequested`. The canary
/// rollout (v0.0.1, no external users) skips a `schemaIntroducedAt`
/// migration marker — older settings files that lack the namespace fall
/// through to `Default::default()` which is also all-`None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OnboardingSettings {
    /// First time `AXIsProcessTrustedWithOptions(prompt: true)` was
    /// invoked. Used to flip the Setup card from `NotRequested` to
    /// `PromptShownNotGranted` once the user has been shown the OS
    /// prompt at least once.
    #[serde(with = "time::serde::rfc3339::option")]
    pub accessibility_prompted_at: Option<OffsetDateTime>,
    /// First time `AXIsProcessTrusted()` was observed as `true`. Sticky:
    /// once set, the Setup card treats a later `false` as
    /// `RevokedAfterGranted` rather than re-entering the prompt
    /// onboarding flow.
    #[serde(with = "time::serde::rfc3339::option")]
    pub accessibility_first_granted_at: Option<OffsetDateTime>,
    /// Timestamp the user (or implicit "everything granted" auto-close)
    /// marked the Setup tab as done. The frontend uses presence to skip
    /// auto-popping the Setup tab on subsequent launches.
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
}

/// AI feature configuration.
///
/// Two independent toggles plus a provider family selector: `enabled` is the
/// master switch for the model-backed AI actions, `semantic_index_enabled`
/// gates the (separate) semantic search index, and `provider` chooses which
/// backend family resolves the actions. Quick actions are always available and
/// are not gated here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    /// Master toggle for all model-backed AI actions. Default `false` (opt-in).
    pub enabled: bool,
    /// Which provider family backs the AI actions. Default `Disabled`.
    pub provider: AiProviderKind,
    /// Per-action allow-list. Empty means "all actions the provider supports".
    pub allowed_actions: Vec<AiActionId>,
    /// Whether streaming output is surfaced in the UI. Default `true`.
    pub allow_streaming: bool,
    /// Per-request timeout in milliseconds. Default `30_000`.
    pub request_timeout_ms: u64,
    /// Whether the semantic search index is enabled (separate from `enabled`).
    pub semantic_index_enabled: bool,
    /// Whether the background embedding indexer only runs while on AC power.
    /// Default `true` so laptops don't drain the battery building embeddings;
    /// turning it off lets the indexer run on battery too.
    pub semantic_index_ac_power_only: bool,
    /// Whether the onboarding banner has been shown and dismissed. Sticky —
    /// not reset when availability changes, so the user's "later" is honoured.
    pub onboarding_dismissed: bool,
    /// Whether to offer an `OpenAI` fallback prompt when the device is not
    /// eligible for Apple Intelligence. Default `true`.
    pub allow_openai_fallback_prompt: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: AiProviderKind::default(),
            allowed_actions: Vec::new(),
            allow_streaming: true,
            request_timeout_ms: 30_000,
            semantic_index_enabled: false,
            semantic_index_ac_power_only: true,
            onboarding_dismissed: false,
            allow_openai_fallback_prompt: true,
        }
    }
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

impl Locale {
    /// The persisted wire tag for this locale, matching the `serde` rename
    /// (`"system"`, `"ja"`, `"zh-Hans"`, …).
    ///
    /// Used to hand the UI-language preference to backends that take a plain
    /// string rather than this enum — notably the AI output-language hint,
    /// where the `system` sentinel is resolved to the OS language downstream.
    #[must_use]
    pub const fn as_tag(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::En => "en",
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::ZhHans => "zh-Hans",
            Self::ZhHant => "zh-Hant",
            Self::De => "de",
            Self::Fr => "fr",
            Self::Es => "es",
        }
    }
}

impl AppSettings {
    /// Validate value-range invariants that the wire format alone cannot
    /// enforce. Run on every entry point that mutates persisted settings —
    /// the storage `save_settings` path, the IPC `UpdateSettings` handler,
    /// and the desktop / CLI startup load — so a hand-edited config file or
    /// a buggy frontend cannot wedge the daemon with values like
    /// `paste_delay_ms = u64::MAX` or `palette_row_count = 0`.
    ///
    /// Besides the scalar ranges this also rejects the structural mistakes
    /// that would otherwise only surface much later (or silently): malformed
    /// auxiliary hotkeys, an empty `capture_kinds` set that would capture
    /// nothing while `capture_enabled` is still on, a degenerate AI request
    /// timeout, and `regex_denylist` patterns that fail to compile under the
    /// same DoS-resistant limits the classifier applies — so a corrupt rule is
    /// caught at the validation boundary rather than when the capture loop
    /// next refreshes settings.
    pub fn validate(&self) -> Result<()> {
        validate_accelerator("global_hotkey", &self.global_hotkey)?;
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
        if self.capture_kinds.is_empty() {
            return Err(AppError::InvalidInput(
                "capture_kinds must list at least one content kind; use capture_enabled=false to pause capture instead".to_owned(),
            ));
        }
        if self.ai.request_timeout_ms == 0 {
            return Err(AppError::InvalidInput(
                "ai.request_timeout_ms must be greater than 0".to_owned(),
            ));
        }
        if self.ai.request_timeout_ms > MAX_AI_REQUEST_TIMEOUT_MS {
            return Err(AppError::InvalidInput(format!(
                "ai.request_timeout_ms must be <= {MAX_AI_REQUEST_TIMEOUT_MS}"
            )));
        }
        // `max_total_bytes` and `max_thumbnail_total_bytes` are intentionally
        // not cross-validated: the desktop UI exposes the former but not the
        // latter (default 64 MiB), so requiring `thumb <= total` would make an
        // otherwise-reasonable small total budget unsavable with no UI knob to
        // fix it. The two budgets govern independent tables and eviction
        // sweeps, so an inverted pair is suboptimal, not incoherent.
        //
        // Auxiliary shortcuts share `global_hotkey`'s accelerator grammar.
        // Empty values mean "unset" (the binding falls back to its built-in
        // default), so they are skipped rather than rejected.
        for (action, accel) in &self.palette_hotkeys {
            if accel.trim().is_empty() {
                continue;
            }
            validate_accelerator(&format!("palette_hotkeys[{action:?}]"), accel)?;
        }
        for (action, accel) in &self.secondary_hotkeys {
            if accel.trim().is_empty() {
                continue;
            }
            validate_accelerator(&format!("secondary_hotkeys[{action:?}]"), accel)?;
        }
        // Cap the rule count before compiling: each pattern runs against every
        // capture, so an unbounded list defeats the per-pattern DoS limits in
        // aggregate.
        if self.regex_denylist.len() > MAX_USER_REGEX_COUNT {
            return Err(AppError::InvalidInput(format!(
                "regex_denylist must have at most {MAX_USER_REGEX_COUNT} patterns"
            )));
        }
        // Compile every denylist pattern under the same length / nesting /
        // DFA-size limits the classifier enforces, mapping the policy-shaped
        // failure onto an input-validation error so a hostile or malformed
        // rule cannot be persisted or loaded.
        for pattern in &self.regex_denylist {
            compile_user_regex(pattern).map_err(|err| match err {
                AppError::Policy(msg) => AppError::InvalidInput(msg),
                other => other,
            })?;
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
    validate_accelerator("global_hotkey", raw)
}

/// Shape-check an accelerator string, attributing any failure to `field` so
/// the error names the setting at fault (`global_hotkey`, a `palette_hotkeys`
/// entry, …). See [`validate_hotkey`] for the format the check enforces.
fn validate_accelerator(field: &str, raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(format!("{field} must not be empty")));
    }
    if trimmed != raw {
        return Err(AppError::InvalidInput(format!(
            "{field} must not have leading/trailing whitespace"
        )));
    }
    let segments: Vec<&str> = trimmed.split('+').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(AppError::InvalidInput(format!(
            "{field} must not contain empty `+` segments"
        )));
    }
    let (key, mods) = segments.split_last().expect("non-empty after trim check");
    let mut seen = std::collections::HashSet::new();
    for m in mods {
        let canonical = canonical_modifier(m)
            .ok_or_else(|| AppError::InvalidInput(format!("{field}: unknown modifier `{m}`")))?;
        if !seen.insert(canonical) {
            return Err(AppError::InvalidInput(format!(
                "{field}: duplicate modifier `{m}`"
            )));
        }
    }
    if canonical_modifier(key).is_some() {
        return Err(AppError::InvalidInput(format!(
            "{field} must end with a non-modifier key"
        )));
    }
    if !is_valid_hotkey_key(key) {
        return Err(AppError::InvalidInput(format!(
            "{field}: invalid key `{key}`"
        )));
    }
    Ok(())
}

fn canonical_modifier(token: &str) -> Option<&'static str> {
    // `meta` / `win` / `windows` / `opt` are kept even though `global-hotkey`'s
    // `parse_hotkey` does not map them: the desktop hotkey recorder emits a
    // literal `Win` for the Meta key on Windows/Linux (see the frontend's
    // `captureFromKeyboardEvent`), so rejecting it here would block saving a
    // recorded binding. Aligning both sides (recorder → `Super`, validator
    // drops these) is a coordinated frontend change tracked separately. All
    // four `CmdOrCtrl` spellings the parser accepts are recognised.
    match token.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "super" | "meta" | "win" | "windows" => Some("super"),
        "ctrl" | "control" => Some("ctrl"),
        "cmdorctrl" | "cmdorcontrol" | "commandorctrl" | "commandorcontrol" => Some("cmdorctrl"),
        "alt" | "option" | "opt" => Some("alt"),
        "shift" => Some("shift"),
        _ => None,
    }
}

fn is_valid_hotkey_key(key: &str) -> bool {
    // Single printable ASCII char (letter/digit/punct), or a named key from
    // the known whitelist. The named set tracks the accept set of `parse_key`
    // in `global-hotkey` (the parser `tauri-plugin-global-shortcut` delegates
    // to via `Shortcut::from_str`) so the validator does not reject a key the
    // OS would accept at register time. Keep this in sync when bumping the
    // crate. (`RETURN` is the lone superset entry kept for the settings UI.)
    if key.chars().count() == 1 {
        let c = key.chars().next().expect("len-checked above");
        return c.is_ascii_alphanumeric() || "`-=[]\\;',./".contains(c);
    }
    let upper = key.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix('F')
        && (1..=3).contains(&rest.len())
        && rest.chars().all(|c| c.is_ascii_digit())
        && let Ok(n) = rest.parse::<u32>()
    {
        return (1..=24).contains(&n);
    }
    // `KeyA`..`KeyZ` and `Digit0`..`Digit9` — the spelled-out forms of the
    // single-char keys handled above. `global-hotkey` accepts both spellings.
    if let Some(rest) = upper.strip_prefix("KEY") {
        return rest.len() == 1 && rest.as_bytes()[0].is_ascii_uppercase();
    }
    if let Some(rest) = upper.strip_prefix("DIGIT") {
        return rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit();
    }
    matches!(
        upper.as_str(),
        // Spelled-out forms of the single-char punctuation keys above.
        "BACKQUOTE"
            | "BACKSLASH"
            | "BRACKETLEFT"
            | "BRACKETRIGHT"
            | "COMMA"
            | "EQUAL"
            | "MINUS"
            | "PERIOD"
            | "QUOTE"
            | "SEMICOLON"
            | "SLASH"
            // Whitespace / editing / navigation.
            | "SPACE"
            | "ENTER"
            | "RETURN"
            | "ESC"
            | "ESCAPE"
            | "TAB"
            | "BACKSPACE"
            | "DELETE"
            | "INSERT"
            | "PAUSE"
            | "PAUSEBREAK"
            | "UP"
            | "DOWN"
            | "LEFT"
            | "RIGHT"
            | "ARROWUP"
            | "ARROWDOWN"
            | "ARROWLEFT"
            | "ARROWRIGHT"
            | "HOME"
            | "END"
            | "PAGEUP"
            | "PAGEDOWN"
            | "CAPSLOCK"
            | "NUMLOCK"
            | "SCROLLLOCK"
            | "PRINTSCREEN"
            // Numeric keypad.
            | "NUMPAD0" | "NUM0"
            | "NUMPAD1" | "NUM1"
            | "NUMPAD2" | "NUM2"
            | "NUMPAD3" | "NUM3"
            | "NUMPAD4" | "NUM4"
            | "NUMPAD5" | "NUM5"
            | "NUMPAD6" | "NUM6"
            | "NUMPAD7" | "NUM7"
            | "NUMPAD8" | "NUM8"
            | "NUMPAD9" | "NUM9"
            | "NUMPADADD" | "NUMADD" | "NUMPADPLUS" | "NUMPLUS"
            | "NUMPADDECIMAL" | "NUMDECIMAL"
            | "NUMPADDIVIDE" | "NUMDIVIDE"
            | "NUMPADENTER" | "NUMENTER"
            | "NUMPADEQUAL" | "NUMEQUAL"
            | "NUMPADMULTIPLY" | "NUMMULTIPLY"
            | "NUMPADSUBTRACT" | "NUMSUBTRACT"
            // Media / volume keys.
            | "AUDIOVOLUMEDOWN" | "VOLUMEDOWN"
            | "AUDIOVOLUMEUP" | "VOLUMEUP"
            | "AUDIOVOLUMEMUTE" | "VOLUMEMUTE"
            | "MEDIAPLAY"
            | "MEDIAPAUSE"
            | "MEDIAPLAYPAUSE"
            | "MEDIASTOP"
            | "MEDIATRACKNEXT"
            | "MEDIATRACKPREV"
            | "MEDIATRACKPREVIOUS"
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
            max_thumbnail_total_bytes: default_max_thumbnail_total_bytes(),
            capture_enabled: true,
            auto_paste_enabled: true,
            paste_format_default: PasteFormat::default(),
            paste_delay_ms: 60,
            app_denylist: password_manager_preset_rules(),
            regex_denylist: Vec::new(),
            ai: AiSettings::default(),
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
            permanent_delete_on_delete: false,
            block_sensitive_captures: false,
            capture_initial_clipboard_on_launch: default_capture_initial_clipboard_on_launch(),
            auto_update_check: default_auto_update_check(),
            update_channel: UpdateChannel::default(),
            onboarding: OnboardingSettings::default(),
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

pub const fn default_max_thumbnail_total_bytes() -> Option<u64> {
    Some(64 * 1024 * 1024)
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
    fn validate_hotkey_accepts_keypad_media_and_named_keys() {
        // These are accepted by `global-hotkey`'s `parse_key`, so the
        // validator must not false-reject them (the previous whitelist did).
        for ok in [
            "Cmd+Numpad5",
            "Cmd+NumpadAdd",
            "Cmd+NumpadEnter",
            "CmdOrCtrl+VolumeUp",
            "Ctrl+MediaPlayPause",
            "Cmd+Pause",
            "Cmd+ArrowUp",
            // Spelled-out forms of single-char keys.
            "Cmd+KeyA",
            "Cmd+Digit0",
            "Cmd+BracketLeft",
            "Cmd+Comma",
            // All four CmdOrCtrl spellings global-hotkey accepts.
            "CmdOrControl+V",
            "CommandOrControl+V",
        ] {
            validate_hotkey(ok).unwrap_or_else(|err| panic!("expected `{ok}` to validate: {err}"));
        }
    }

    #[test]
    fn validate_rejects_too_many_regex_denylist_patterns() {
        let settings = AppSettings {
            regex_denylist: vec!["a".to_owned(); MAX_USER_REGEX_COUNT + 1],
            ..AppSettings::default()
        };
        assert!(matches!(
            settings.validate(),
            Err(AppError::InvalidInput(_))
        ));
        // The cap itself must remain valid.
        let at_cap = AppSettings {
            regex_denylist: vec!["a".to_owned(); MAX_USER_REGEX_COUNT],
            ..AppSettings::default()
        };
        at_cap
            .validate()
            .expect("rule count at the cap must validate");
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

    #[test]
    fn validate_accepts_defaults() {
        AppSettings::default()
            .validate()
            .expect("default settings must validate");
    }

    #[test]
    fn deserialize_app_denylist_keeps_legacy_and_typed_rules() {
        let json = r#"[
            "1Password",
            { "type": "pattern", "value": "Secrets" },
            { "type": "source_app", "kind": "macos_bundle_id", "value": "com.example.app", "label": "Example", "source": "preset" }
        ]"#;
        let rules: Vec<AppDenyRule> =
            deserialize_app_denylist(&mut serde_json::Deserializer::from_str(json))
                .expect("mixed shapes deserialize");
        assert_eq!(
            rules,
            vec![
                AppDenyRule::Pattern {
                    value: "1Password".to_owned()
                },
                AppDenyRule::Pattern {
                    value: "Secrets".to_owned()
                },
                AppDenyRule::SourceApp {
                    kind: SourceAppIdKind::MacosBundleId,
                    value: "com.example.app".to_owned(),
                    label: Some("Example".to_owned()),
                    source: RuleSource::Preset,
                },
            ]
        );
    }

    #[test]
    fn deserialize_app_denylist_skips_unreadable_rule() {
        // A rule shape this build cannot parse (e.g. a future variant seen
        // after a downgrade, or a hand-edited row) must be skipped rather
        // than aborting the whole settings deserialize and dropping every
        // surviving rule.
        let json = r#"[
            "1Password",
            { "type": "future_variant", "value": "x" },
            { "type": "pattern", "value": "Secrets" }
        ]"#;
        let rules: Vec<AppDenyRule> =
            deserialize_app_denylist(&mut serde_json::Deserializer::from_str(json))
                .expect("one bad rule must not fail the whole list");
        assert_eq!(
            rules,
            vec![
                AppDenyRule::Pattern {
                    value: "1Password".to_owned()
                },
                AppDenyRule::Pattern {
                    value: "Secrets".to_owned()
                },
            ]
        );
    }

    #[test]
    fn app_settings_deserialize_survives_bad_denylist_rule() {
        let settings = AppSettings {
            app_denylist: vec![AppDenyRule::Pattern {
                value: "keep-me".to_owned(),
            }],
            ..AppSettings::default()
        };
        let mut value = serde_json::to_value(&settings).expect("settings serialize");
        // Splice an unreadable rule into the persisted blob alongside a good
        // one and confirm the whole `AppSettings` still deserializes.
        value["app_denylist"] = serde_json::json!([
            { "type": "future_variant", "value": "x" },
            { "type": "pattern", "value": "keep-me" }
        ]);
        let restored: AppSettings = serde_json::from_value(value).expect("settings deserialize");
        assert_eq!(
            restored.app_denylist,
            vec![AppDenyRule::Pattern {
                value: "keep-me".to_owned()
            }]
        );
    }

    #[test]
    fn validate_rejects_empty_capture_kinds() {
        let mut settings = AppSettings::default();
        settings.capture_kinds.clear();
        assert!(matches!(
            settings.validate(),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn validate_rejects_degenerate_ai_timeout() {
        let mut settings = AppSettings::default();
        settings.ai.request_timeout_ms = 0;
        assert!(matches!(
            settings.validate(),
            Err(AppError::InvalidInput(_))
        ));

        settings.ai.request_timeout_ms = MAX_AI_REQUEST_TIMEOUT_MS + 1;
        assert!(matches!(
            settings.validate(),
            Err(AppError::InvalidInput(_))
        ));

        settings.ai.request_timeout_ms = MAX_AI_REQUEST_TIMEOUT_MS;
        settings
            .validate()
            .expect("the boundary value must validate");
    }

    #[test]
    fn validate_allows_thumbnail_budget_above_total() {
        // The two byte budgets are not cross-validated (the UI exposes only
        // `max_total_bytes`), so a small total with the default thumbnail cap
        // must still save.
        let settings = AppSettings {
            max_total_bytes: Some(1_000),
            max_thumbnail_total_bytes: Some(64 * 1024 * 1024),
            ..AppSettings::default()
        };
        settings
            .validate()
            .expect("independent byte budgets must validate");
    }

    #[test]
    fn validate_rejects_uncompilable_regex_denylist() {
        let settings = AppSettings {
            regex_denylist: vec!["valid".to_owned(), "(".to_owned()],
            ..AppSettings::default()
        };
        assert!(matches!(
            settings.validate(),
            Err(AppError::InvalidInput(_))
        ));
    }

    #[test]
    fn validate_checks_auxiliary_hotkeys_but_skips_empty() {
        let mut settings = AppSettings::default();
        // A malformed binding is rejected and attributed to the field.
        settings
            .palette_hotkeys
            .insert(PaletteHotkeyAction::Pin, "Cmd+".to_owned());
        match settings.validate() {
            Err(AppError::InvalidInput(msg)) => {
                assert!(msg.contains("palette_hotkeys"), "got: {msg}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }

        // An empty value means "unset" and is skipped, not rejected.
        let mut settings = AppSettings::default();
        settings
            .secondary_hotkeys
            .insert(SecondaryHotkeyAction::RepasteLast, String::new());
        settings
            .validate()
            .expect("empty auxiliary hotkey must be treated as unset");
    }
}

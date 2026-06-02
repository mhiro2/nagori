pub mod errors;
pub mod factory;
pub mod image_signature;
pub mod limits;
pub mod model;
pub mod policy;
pub mod repositories;
pub mod services;
pub mod settings;
pub mod text;

pub use errors::{AppError, PasteFailureReason, Result};
pub use factory::EntryFactory;
pub use image_signature::{
    ImageFormat, SUPPORTED_IMAGE_MIMES, detect as detect_image_signature, matches_declared_mime,
};
pub use limits::{MAX_DECODED_IMAGE_PIXELS, MAX_ENTRY_SIZE_BYTES, MAX_IPC_BYTES};
pub use model::*;
pub use policy::{
    MAX_USER_REGEX_LEN, MAX_USER_REGEX_NESTING, SecretAction, SensitivityClassification,
    SensitivityClassifier, compile_user_regex,
};
pub use repositories::{AuditLog, EntryRepository, SearchRepository, SettingsRepository};
pub use services::{
    FtsCandidate, NgramCandidate, NgramQueryMode, Ranker, SearchCandidateProvider, SearchPlan,
    SearchService,
};
pub use settings::{
    AiSettings, AppDenyRule, AppSettings, Appearance, Locale, MAX_PALETTE_ROW_COUNT,
    MAX_PASTE_DELAY_MS, MAX_RETENTION_COUNT, MAX_RETENTION_DAYS, OnboardingSettings,
    PASSWORD_MANAGER_PRESET, PaletteHotkeyAction, PasteFormat, PresetEntry, RecentOrder,
    RuleSource, SecondaryHotkeyAction, SecretHandling, SourceAppIdKind, UpdateChannel,
    password_manager_preset_rules, validate_hotkey,
};
pub use text::{has_cjk, normalize_text};

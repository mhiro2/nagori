pub mod errors;
pub mod factory;
pub mod limits;
pub mod model;
pub mod policy;
pub mod repositories;
pub mod services;
pub mod settings;
pub mod text;

pub use errors::{AppError, Result};
pub use factory::EntryFactory;
pub use limits::{MAX_ENTRY_SIZE_BYTES, MAX_IPC_BYTES};
pub use model::*;
pub use policy::{
    MAX_USER_REGEX_LEN, MAX_USER_REGEX_NESTING, SecretAction, SensitivityClassification,
    SensitivityClassifier, compile_user_regex,
};
pub use repositories::{AuditLog, EntryRepository, SearchRepository, SettingsRepository};
pub use services::{
    FtsCandidate, NgramCandidate, Ranker, SearchCandidateProvider, SearchPlan, SearchService,
};
pub use settings::{
    AppSettings, Appearance, Locale, MAX_PALETTE_ROW_COUNT, MAX_PASTE_DELAY_MS,
    MAX_RETENTION_COUNT, MAX_RETENTION_DAYS, PaletteHotkeyAction, PasteFormat, RecentOrder,
    SecondaryHotkeyAction, SecretHandling, validate_hotkey,
};
pub use text::normalize_text;

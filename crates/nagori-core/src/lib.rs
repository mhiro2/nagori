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
pub use policy::{SecretAction, SensitivityClassification, SensitivityClassifier};
pub use repositories::{AuditLog, EntryRepository, SearchRepository, SettingsRepository};
pub use services::{
    FtsCandidate, NgramCandidate, Ranker, SearchCandidateProvider, SearchPlan, SearchService,
};
pub use settings::{AppSettings, Appearance, Locale, PasteFormat, RecentOrder, SecretHandling};
pub use text::normalize_text;

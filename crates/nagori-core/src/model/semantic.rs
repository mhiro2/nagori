//! Shared vocabulary for the on-device semantic search index.
//!
//! The vectors themselves live in `nagori-storage`; the embedder that produces
//! them lives behind the `nagori-ai` `Embedder` trait. These types are the
//! contract both sides agree on: what model produced the stored vectors
//! ([`SemanticIndexMeta`]) and what the live index looks like to the UI /
//! `nagori doctor` ([`SemanticIndexStatus`]).

use serde::{Deserialize, Serialize};

/// Describes the embedding model that produced the vectors currently stored in
/// the semantic index.
///
/// Persisted alongside the vectors so a model / revision / dimension change can
/// be detected at startup and the index rebuilt, rather than silently comparing
/// query vectors against stored vectors from an incompatible embedding space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticIndexMeta {
    /// Opaque model identifier reported by the embedder at runtime.
    pub model_identifier: String,
    /// Model revision reported at runtime. A bump means the embedding space
    /// changed even if the identifier did not.
    pub revision: u32,
    /// Vector dimensionality, read from the model at runtime (never baked).
    pub dimension: u32,
    /// Token cap the embedder applies before silently truncating; the indexer
    /// chunks longer inputs rather than letting the model drop the tail.
    pub max_sequence_length: u32,
    /// Languages the model covers, as runtime-reported locale identifiers.
    pub languages: Vec<String>,
    /// Bumped by the indexing pipeline when its content shaping changes in a
    /// way that invalidates previously-stored vectors for the *same* model.
    pub index_version: u32,
    /// Fingerprint of the privacy policy the stored vectors were embedded
    /// under ([`crate::AppSettings::semantic_policy_hash`]). A mismatch with
    /// the live settings means some stored vector may embed content the
    /// current policy forbids (e.g. a `regex_denylist` rule added after the
    /// entry was embedded), so the index must be purged and rebuilt.
    ///
    /// Defaults to `""` when absent (rows written before the field existed);
    /// the empty value never matches a live fingerprint, so pre-existing
    /// indexes are rebuilt once under the tracked policy.
    #[serde(default)]
    pub policy_hash: String,
}

impl SemanticIndexMeta {
    /// Whether vectors produced under `self` may be compared against vectors
    /// produced under `other`. A mismatch means the stored index must be
    /// cleared and rebuilt before serving semantic queries.
    ///
    /// `max_sequence_length` and `languages` are descriptive, not part of the
    /// embedding-space identity, so they do not gate compatibility.
    /// `policy_hash` *does* gate it: vectors embedded under an older privacy
    /// policy may carry content the current policy forbids, so they must not
    /// be served (or kept) once the policy changes.
    #[must_use]
    pub fn is_compatible_with(&self, other: &Self) -> bool {
        self.model_identifier == other.model_identifier
            && self.revision == other.revision
            && self.dimension == other.dimension
            && self.index_version == other.index_version
            && self.policy_hash == other.policy_hash
    }
}

/// Coarse state of the semantic index, surfaced to the settings UI and
/// `nagori doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticIndexState {
    /// The semantic index toggle is off.
    Disabled,
    /// No embedder backend is wired on this host (everything but macOS today).
    Unsupported,
    /// Enabled, but the embedder is currently unavailable (Apple Intelligence
    /// off, embedding assets missing, device ineligible, …).
    Unavailable,
    /// Enabled and fully up to date — every embeddable entry has a vector.
    Ready,
    /// Enabled and actively (re)building embeddings in the background.
    Indexing,
    /// Enabled but the background indexer is paused by a guard (on battery
    /// while AC-only is set, rate limited, …).
    Paused,
}

/// Live snapshot of the semantic index for the UI / doctor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticIndexStatus {
    pub state: SemanticIndexState,
    /// Entries with an up-to-date embedding.
    pub indexed: u64,
    /// Embeddable entries still waiting for a vector.
    pub pending: u64,
    /// Total live, embeddable entries.
    pub total: u64,
    /// The model the stored vectors were produced with, if any are stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<SemanticIndexMeta>,
}

impl SemanticIndexStatus {
    /// A status for a host with no embedder backend wired.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self {
            state: SemanticIndexState::Unsupported,
            indexed: 0,
            pending: 0,
            total: 0,
            model: None,
        }
    }

    /// A status for the semantic index toggle being off.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            state: SemanticIndexState::Disabled,
            indexed: 0,
            pending: 0,
            total: 0,
            model: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(model: &str, revision: u32, dimension: u32, index_version: u32) -> SemanticIndexMeta {
        SemanticIndexMeta {
            model_identifier: model.to_owned(),
            revision,
            dimension,
            max_sequence_length: 256,
            languages: vec!["en".to_owned(), "ja".to_owned()],
            index_version,
            policy_hash: "policy-a".to_owned(),
        }
    }

    #[test]
    fn identical_metas_are_compatible() {
        let a = meta("model-a", 1, 512, 1);
        let b = meta("model-a", 1, 512, 1);
        assert!(a.is_compatible_with(&b));
    }

    #[test]
    fn revision_or_dimension_or_model_or_index_version_change_is_incompatible() {
        let base = meta("model-a", 1, 512, 1);
        assert!(!base.is_compatible_with(&meta("model-b", 1, 512, 1)));
        assert!(!base.is_compatible_with(&meta("model-a", 2, 512, 1)));
        assert!(!base.is_compatible_with(&meta("model-a", 1, 384, 1)));
        assert!(!base.is_compatible_with(&meta("model-a", 1, 512, 2)));
    }

    #[test]
    fn policy_hash_change_is_incompatible() {
        // A privacy-policy edit (regex_denylist, app_denylist, OTP detection,
        // size ceiling) must invalidate the stored vectors even when the model
        // itself is unchanged: they may embed content the new policy forbids.
        let base = meta("model-a", 1, 512, 1);
        let mut other_policy = meta("model-a", 1, 512, 1);
        other_policy.policy_hash = "policy-b".to_owned();
        assert!(!base.is_compatible_with(&other_policy));

        // Rows persisted before the field existed deserialize to `""`, which
        // never matches a live fingerprint — the old index rebuilds once.
        let mut legacy = meta("model-a", 1, 512, 1);
        legacy.policy_hash = String::new();
        assert!(!legacy.is_compatible_with(&base));
    }

    #[test]
    fn descriptive_fields_do_not_gate_compatibility() {
        let mut a = meta("model-a", 1, 512, 1);
        let mut b = meta("model-a", 1, 512, 1);
        a.max_sequence_length = 256;
        b.max_sequence_length = 128;
        a.languages = vec!["en".to_owned()];
        b.languages = vec!["fr".to_owned()];
        assert!(a.is_compatible_with(&b));
    }
}

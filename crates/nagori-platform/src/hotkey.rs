use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

use async_trait::async_trait;
use nagori_core::Result;
use serde::{Deserialize, Serialize};

/// A global hotkey: a set of modifiers plus a primary key.
///
/// Equality and hashing treat `modifiers` as an unordered, duplicate-free
/// **set**: `[Command, Shift] + P` and `[Shift, Command] + P` are the same
/// hotkey, and a stray repeated modifier does not produce a distinct key.
/// The field stays a `Vec` so callers (and the serialized form) keep the
/// listing order they built, but two hotkeys that bind the same chord compare
/// equal and land in the same `HashMap` / `HashSet` slot regardless of how the
/// modifiers were ordered. Without this, an accelerator parsed as
/// `Cmd+Shift+P` would silently fail to match one registered as `Shift+Cmd+P`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotkey {
    pub modifiers: Vec<HotkeyModifier>,
    pub key: String,
}

impl Hotkey {
    /// The modifiers as an order- and duplicate-independent canonical set,
    /// used as the basis for equality and hashing.
    fn modifier_set(&self) -> BTreeSet<HotkeyModifier> {
        self.modifiers.iter().copied().collect()
    }
}

impl PartialEq for Hotkey {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.modifier_set() == other.modifier_set()
    }
}

impl Eq for Hotkey {}

impl Hash for Hotkey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash the canonical (sorted, deduplicated) modifier set so equal
        // hotkeys hash equally regardless of the order the modifiers were
        // listed in — a `Hash`/`Eq` mismatch would break `HashMap` lookups.
        for modifier in self.modifier_set() {
            modifier.hash(state);
        }
        self.key.hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HotkeyModifier {
    Command,
    Control,
    Option,
    Shift,
    Alt,
    Super,
}

#[async_trait]
pub trait HotkeyManager: Send + Sync {
    /// Register a global hotkey.
    ///
    /// The daemon-side adapters return [`AppError::Unsupported`] because
    /// registration is owned by the Tauri shell's global-shortcut plugin;
    /// failing loudly keeps a daemon caller from silently duplicating the
    /// shell's binding.
    ///
    /// [`AppError::Unsupported`]: nagori_core::AppError::Unsupported
    async fn register(&self, hotkey: Hotkey) -> Result<()>;

    /// Unregister a previously registered hotkey.
    ///
    /// This is a tolerant, idempotent operation: unregistering a hotkey that
    /// was never registered succeeds rather than erroring. The daemon-side
    /// adapters never register anything (see [`Self::register`]), so their
    /// `unregister` is a deliberate no-op `Ok(())` — there is nothing to tear
    /// down, and asymmetry with `register` is intentional rather than an
    /// oversight: teardown of an absent binding is not a failure.
    async fn unregister(&self, hotkey: Hotkey) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn hash_of(hotkey: &Hotkey) -> u64 {
        let mut hasher = DefaultHasher::new();
        hotkey.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn modifier_order_does_not_affect_equality_or_hash() {
        let a = Hotkey {
            modifiers: vec![HotkeyModifier::Command, HotkeyModifier::Shift],
            key: "P".to_owned(),
        };
        let b = Hotkey {
            modifiers: vec![HotkeyModifier::Shift, HotkeyModifier::Command],
            key: "P".to_owned(),
        };
        assert_eq!(a, b, "modifier order must not change equality");
        assert_eq!(hash_of(&a), hash_of(&b), "equal hotkeys must hash equally");
    }

    #[test]
    fn duplicate_modifiers_collapse() {
        let a = Hotkey {
            modifiers: vec![HotkeyModifier::Command, HotkeyModifier::Command],
            key: "V".to_owned(),
        };
        let b = Hotkey {
            modifiers: vec![HotkeyModifier::Command],
            key: "V".to_owned(),
        };
        assert_eq!(
            a, b,
            "a repeated modifier must not produce a distinct chord"
        );
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn different_chords_stay_distinct() {
        let cmd_p = Hotkey {
            modifiers: vec![HotkeyModifier::Command],
            key: "P".to_owned(),
        };
        let shift_p = Hotkey {
            modifiers: vec![HotkeyModifier::Shift],
            key: "P".to_owned(),
        };
        assert_ne!(
            cmd_p, shift_p,
            "distinct modifier sets must not compare equal"
        );
    }
}

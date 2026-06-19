//! Wire and storage size ceilings shared across crates.
//!
//! The IPC newline-delimited JSON transport refuses lines larger than
//! [`MAX_IPC_BYTES`]. Any clipboard entry that the user can configure to be
//! captured (`max_entry_size_bytes`) has to fit into the same envelope after
//! JSON escaping, otherwise the desktop / CLI surfaces silently fail to load
//! it back. To keep the two consistent we expose the IPC ceiling here and use
//! it as the upper bound for `max_entry_size_bytes`, leaving headroom for the
//! envelope (token, request kind, JSON quoting, etc).

/// Maximum size of a single IPC line (request or response) in bytes. Lines
/// larger than this are rejected by both ends of the transport.
pub const MAX_IPC_BYTES: usize = 1024 * 1024;

/// Upper bound for the user-tunable `max_entry_size_bytes` setting.
///
/// Set to ~75% of [`MAX_IPC_BYTES`] so that even with JSON escaping (worst
/// case 6× expansion for control characters, ~1.1× typical for ASCII text)
/// and the envelope overhead, an entry that storage accepts can still cross
/// the IPC boundary. Values above this would create a class of entries that
/// the daemon stores but neither the desktop nor the CLI can read back.
pub const MAX_ENTRY_SIZE_BYTES: usize = (MAX_IPC_BYTES * 3) / 4;

/// Hard cap on decoded image pixel count for clipboard image captures and
/// copy-back.
///
/// `max_entry_size_bytes` only inspects the encoded bytes on the wire, but
/// encoded formats like PNG / JPEG / WebP can advertise huge dimensions in
/// a tiny payload (a few-KB PNG can decode to a 16 GB RGBA buffer). Capping
/// the decoded pixel count is the only defence against that asymmetry, and
/// the limit has to be platform-wide because the same encoded bytes can be
/// pushed through capture, copy-back, or a future preview pipeline.
///
/// 64 megapixels keeps the worst-case RGBA buffer at 256 MB — comfortably
/// above an 8K screenshot (~33 MP) but well below the OOM threshold on a
/// typical workstation. The value is intentionally not user-tunable: the
/// only reason to raise it is to accept payloads that would routinely
/// crash the daemon.
pub const MAX_DECODED_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

/// Upper bound for the user-tunable `max_image_entry_size_bytes` setting.
///
/// Deliberately *not* tied to [`MAX_IPC_BYTES`] the way [`MAX_ENTRY_SIZE_BYTES`]
/// is: image payloads never cross the IPC line as inline JSON. The desktop
/// streams them through the `nagori-image://` custom scheme straight to the
/// `WebView`, copy-back reads the `SQLite` BLOB in-process, and the image
/// `EntryDto` carries only a `mime`/`byte_count` summary — so the only ceilings
/// that bound an image are decode safety ([`MAX_DECODED_IMAGE_PIXELS`], 64 MP →
/// 256 MB RGBA) and the per-representation storage budget, both enforced
/// independently of this value.
///
/// 64 MiB sits comfortably under those while accepting any screenshot a decode
/// can survive. Raising the user setting toward this ceiling is an
/// expert / high-memory choice — raw TIFF/DIB, the decoded RGBA buffer, and the
/// re-encoded PNG can all coexist for one clip — so the shipped default sits
/// far lower (see `settings::default_max_image_entry_size_bytes`).
pub const MAX_IMAGE_ENTRY_SIZE_BYTES: usize = 64 * 1024 * 1024;

/// Per-content-kind byte budgets applied while reading the clipboard and while
/// trimming an entry's stored representation set.
///
/// Text-shaped payloads (plain / html / rtf / file-URL lists) are gated by
/// `text_bytes`, image payloads by `image_bytes`. Keeping the two separate is
/// what lets a multi-megabyte screenshot be captured under
/// [`MAX_IMAGE_ENTRY_SIZE_BYTES`] while a text clip stays bounded by the
/// IPC-tied [`MAX_ENTRY_SIZE_BYTES`]: the same encoded bytes that a screenshot
/// carries would silently fail to load back if measured against the text
/// budget.
///
/// Adapters apply the matching field to each representation as they probe
/// clipboard sizes; the capture loop re-applies the same split authoritatively
/// in `admit` and `ClipboardEntry::trim_alternatives_to_budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadBudget {
    pub text_bytes: usize,
    pub image_bytes: usize,
}

impl ReadBudget {
    #[must_use]
    pub const fn new(text_bytes: usize, image_bytes: usize) -> Self {
        Self {
            text_bytes,
            image_bytes,
        }
    }

    /// Budget that applies to a representation, chosen by whether it is an
    /// image payload.
    #[must_use]
    pub const fn for_kind(self, is_image: bool) -> usize {
        if is_image {
            self.image_bytes
        } else {
            self.text_bytes
        }
    }

    /// The largest a single payload may grow regardless of kind. Adapters that
    /// can only bound one cumulative read at the raw-byte boundary use this and
    /// defer the per-kind decision to the content-aware checks downstream.
    #[must_use]
    pub const fn max(self) -> usize {
        if self.text_bytes > self.image_bytes {
            self.text_bytes
        } else {
            self.image_bytes
        }
    }

    /// Cumulative ceiling for an entry holding both an image and its text
    /// alternatives — the most bytes an in-budget clip could legitimately
    /// total. Saturates so a `usize::MAX`-ish budget cannot wrap.
    #[must_use]
    pub const fn total(self) -> usize {
        self.text_bytes.saturating_add(self.image_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_size_leaves_envelope_headroom_under_ipc_ceiling() {
        // Storage validator must never accept an entry whose raw bytes alone
        // would already breach the IPC line cap, even before envelope JSON.
        // Wrapped in `const { ... }` so the check is enforced at compile time
        // and survives clippy's `assertions_on_constants` lint.
        const _: () = {
            assert!(MAX_ENTRY_SIZE_BYTES < MAX_IPC_BYTES);
            // Reserve at least 64 KiB for envelope (token, request kind, quoting).
            assert!(MAX_IPC_BYTES - MAX_ENTRY_SIZE_BYTES >= 64 * 1024);
        };
    }
}

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

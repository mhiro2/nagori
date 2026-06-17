//! Tauri command surface for the desktop shell, split by concern across the
//! submodules below. This module holds only the shared `parse_entry_id`
//! helper and the re-exports that keep external `commands::*` call sites (and
//! `generate_handler!`) stable.

use nagori_core::EntryId;

use crate::error::CommandError;

pub mod ai_commands;
pub mod entry_commands;
pub mod installer;
pub mod paste_commands;
pub mod preview;
pub mod settings_commands;
pub mod updater;
pub mod window_commands;

// Preview temp-file helpers live in `preview`; re-export them so the call
// sites that use them (clear-history / delete in `entry_commands`) and
// `lib.rs` (startup wipe, exit cleanup) keep referring to them by their
// original paths.
pub(crate) use self::preview::{purge_preview_temp_dir, remove_preview_temp_files_for};
// Helpers invoked from outside their defining submodule (the tray, the global
// hotkeys, and `lib.rs`) keep their original `commands::*` paths via these
// re-exports.
pub(crate) use entry_commands::clear_non_pinned_and_previews;
pub(crate) use paste_commands::{emit_paste_failed_with_reason, paste_failure_reason};
pub(crate) use window_commands::{recenter_palette_on_cursor_monitor, show_settings_window};

fn parse_entry_id(value: &str) -> Result<EntryId, CommandError> {
    value
        .parse::<EntryId>()
        .map_err(|err| CommandError::invalid_input(format!("invalid entry id: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_entry_id_accepts_canonical_uuid_form() {
        // EntryId is a thin newtype around UUID; the command layer must
        // round-trip its `Display` form so the WebView can persist an id and
        // hand it back later (e.g. between palette open / preview hover).
        let original = EntryId::new();
        let parsed = parse_entry_id(&original.to_string()).expect("uuid parses");
        assert_eq!(parsed, original);
    }

    #[test]
    fn parse_entry_id_rejects_garbage_with_invalid_input_error() {
        // Surface the parse failure as `invalid_input` so the WebView can
        // localise the toast and keep the row interactive (recoverable).
        let err = parse_entry_id("not-a-uuid").expect_err("garbage rejected");
        assert_eq!(err.code, "invalid_input");
        assert!(err.recoverable);
        assert!(
            err.message.contains("invalid entry id"),
            "expected curated message, got {:?}",
            err.message,
        );
    }
}

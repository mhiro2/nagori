# Tauri command surface

Every `#[tauri::command]` the desktop shell registers, classified by effect.
This is the audit companion to the per-window ACL described in
[ARCHITECTURE.md §19](../ARCHITECTURE.md#19-threat-model--security-decisions):
`build.rs` declares the manifest (deny-by-default), `generate_handler!` in
`apps/desktop/src-tauri/src/lib.rs` registers the handlers, and
`capabilities/palette.json` / `capabilities/settings.json` grant a subset per
window.

The command **set** and the per-window split are guarded mechanically:
`apps/desktop/src-tauri/tests/command_surface.rs` asserts that the manifest and
`generate_handler!` list the same commands, that every granted permission (in
any `capabilities/*.json`) maps to a real command, and that the palette and
settings windows share only the audited read-only commands — so adding or moving
a command without updating that test fails CI. The effect **classification** in
the table below is maintained by hand; keep it in sync when you touch the command
surface.

## Window exposure

- **palette** — the `main` palette webview (`capabilities/palette.json`).
- **settings** — the `settings` webview (`capabilities/settings.json`).
- **internal** — in the manifest but granted to no webview: invoked only from
  Rust (hotkeys, tray, IPC, the embedded daemon) and shipped with no
  `commands.ts` binding, so the JS invoke surface matches the granted set.

## Read-only

Queries and on-device computation. No stored data is created, mutated, or
deleted; nothing leaves the machine.

| Command | Window | Notes |
| --- | --- | --- |
| `search_clipboard` | palette | Ranked search over history. |
| `list_recent_entries` | internal | Recency list (CLI / headless). |
| `list_pinned_entries` | internal | Pinned list (CLI / headless). |
| `get_entry` | internal | Fetch one entry (CLI / headless). |
| `list_paste_options` | palette | MIME types for the paste-as picker. |
| `get_entry_preview` | palette | Bounded preview with query highlights. |
| `get_entry_preview_full` | palette | Expanded preview, `Public` entries only. |
| `get_settings` | palette + settings | Settings snapshot with a revision token. |
| `password_manager_preset` | settings | Canonical password-manager block-list. |
| `data_dir_sync_warning` | settings | Warns if the data directory is inside a cloud-sync folder. |
| `get_permissions` | palette + settings | OS permission status. |
| `get_capabilities` | palette + settings | Per-OS capability matrix. |
| `last_hotkey_failure` | palette + settings | Cached hotkey-registration failure. |
| `get_ai_availability` | palette + settings | Point-in-time AI provider status. |
| `get_semantic_index_status` | settings | Semantic index build progress. |
| `cli_install_status` | settings | Whether the CLI is linked onto `PATH`. |
| `run_quick_action` | palette | Deterministic text transform; returns text, persists nothing. |
| `start_ai_action` | palette | Streams on-device model output via events; persists nothing (`save_ai_result` promotes it). |
| `cancel_ai_action` | palette | Cancels an in-flight streaming run. |

## Write

Mutates local state — the clipboard, history rows, pins, or settings.

| Command | Window | Notes |
| --- | --- | --- |
| `copy_entry` | internal | Write an entry to the system clipboard; bump use-count. |
| `copy_entry_from_palette` | palette | Copy, hide the palette, restore source focus. |
| `copy_entries_combined` | palette | Join selected text and copy it. **Also inserts a new history entry** for the joined text (surfaced in the palette's multi-select hint). |
| `add_entry` | internal | Insert a manually-supplied text entry. |
| `pin_entry` | palette | Toggle an entry's pin flag. |
| `set_capture_enabled` | palette | Persist the capture pause/resume toggle. |
| `update_settings` | settings | Persist settings (compare-and-swap on the revision token). |
| `save_ai_result` | palette | Promote a generated AI result to a new history entry. |
| `rebuild_semantic_index` | settings | Signal the worker to re-embed the corpus. |

## Destructive

Deletes history data.

| Command | Window | Notes |
| --- | --- | --- |
| `delete_entry` | palette | Soft-delete one entry (`Secret` is hard-deleted; see [privacy.md](privacy.md)). |
| `delete_entries` | palette | Bulk soft-delete. |
| `purge_deleted_entries` | settings | Hard-delete already soft-deleted rows now. |
| `clear_history` | internal | Soft-delete every non-pinned entry (tray / hotkey). |

## External side effect

Crosses the app boundary — synthetic input into another app, the browser, an OS
prompt, the filesystem outside the database, or the network.

| Command | Window | Notes |
| --- | --- | --- |
| `paste_entry` | internal | Copy, then synthesize ⌘V into the foreground app. |
| `paste_entry_from_palette` | palette | Copy, restore source focus, synthesize ⌘V. |
| `paste_entry_representation_from_palette` | palette | Copy a chosen MIME type, then synthesize ⌘V. |
| `repaste_last` | internal | Re-synthesize the most recent paste. |
| `open_url_external` | palette | Open a `Public` URL entry in the default browser (scheme allowlist + sensitivity gate). |
| `preview_entry` | palette | Open OS-native Quick Look (macOS only, `Public` only); writes a temp preview file. |
| `request_accessibility` | settings | Trigger the OS Accessibility prompt / open System Settings. |
| `install_cli` | settings | Symlink the bundled CLI under `~/.local/bin` (Unix). |
| `check_for_updates` | settings | Probe the update endpoint (network GET). |

## Window / lifecycle

Shows, hides, or focuses a window. No data effect.

| Command | Window | Notes |
| --- | --- | --- |
| `open_palette` | internal | Snapshot the frontmost app, show/focus the palette. |
| `close_palette` | palette | Hide the palette, clear the frontmost snapshot. |
| `toggle_palette` | internal | Toggle palette visibility (hotkey / tray). |
| `hide_palette` | palette | Hide the palette, clear the frontmost snapshot. |
| `open_settings` | palette | Show the settings window on a route. |
| `close_settings` | internal | Hide the settings window. |

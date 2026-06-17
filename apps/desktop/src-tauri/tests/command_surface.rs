//! Static guard over the Tauri command surface.
//!
//! Three sources must agree or the per-window ACL silently breaks:
//!   - `build.rs` enumerates every command so `tauri-build` generates an
//!     `allow-<cmd>` / `deny-<cmd>` permission and flips the surface to
//!     deny-by-default,
//!   - `generate_handler!` in `lib.rs` registers the actual handlers,
//!   - `capabilities/*.json` grant a subset per window (`palette.json` →
//!     `main`, `settings.json` → `settings`, `default.json` → both).
//!
//! A `#[tauri::command]` added to `generate_handler!` but forgotten in
//! `build.rs` has no generated permission (every webview invocation is
//! rejected); the reverse leaves a dead manifest entry. A capability that
//! grants a misspelled or removed command fails silently at runtime. And a
//! command granted to *both* windows widens the surface a compromised webview
//! can reach. These run as a plain `cargo test` (no webview), so they verify
//! the ACL *configuration* the Tauri runtime enforces — the mechanical check
//! that future command additions can't skip.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `"snake_case"` string literal inside `build.rs`'s `.commands(&[ … ])`
/// manifest array.
fn build_rs_commands() -> BTreeSet<String> {
    let src = fs::read_to_string(manifest_dir().join("build.rs")).expect("read build.rs");
    let start = src
        .find(".commands(&[")
        .expect("build.rs declares a `.commands(&[` manifest");
    let rest = &src[start..];
    let end = rest
        .find("])")
        .expect("build.rs `.commands(&[` array is closed with `])`");
    string_literals(&rest[..end])
}

/// Final path segment of every command registered in `lib.rs`'s
/// `generate_handler![ … ]` (`commands::preview::get_entry_preview` ->
/// `get_entry_preview`).
fn generate_handler_commands() -> BTreeSet<String> {
    let src = fs::read_to_string(manifest_dir().join("src/lib.rs")).expect("read lib.rs");
    let start = src
        .find("generate_handler![")
        .expect("lib.rs registers commands via `generate_handler![`");
    let rest = &src[start..];
    let end = rest
        .find(']')
        .expect("`generate_handler![` list is closed with `]`");
    rest[..end]
        .split(',')
        .filter_map(|item| item.rsplit("::").next())
        .map(str::trim)
        .filter(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(str::to_owned)
        .collect()
}

/// One capability file's app-command grants: its name, the window labels it
/// binds to, and the command names it allows (every `allow-<cmd>` with the
/// prefix stripped and dashes folded back to underscores; non-`allow-`
/// permissions like `core:default` are ignored).
struct Capability {
    file: String,
    windows: Vec<String>,
    commands: BTreeSet<String>,
}

/// Every `*.json` under `capabilities/`, parsed. Iterating the whole directory
/// (rather than naming `palette.json` / `settings.json`) keeps the guard honest
/// when a grant lands in `default.json` — which binds to *both* windows — or in
/// a capability file added later.
fn capabilities() -> Vec<Capability> {
    let dir = manifest_dir().join("capabilities");
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).expect("read capabilities dir") {
        let path = entry.expect("capabilities dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let raw = fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {file}: {err}"));
        let json: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|err| panic!("parse {file}: {err}"));
        let windows = json["windows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        let commands = json["permissions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .filter_map(|perm| perm.strip_prefix("allow-"))
            .map(|cmd| cmd.replace('-', "_"))
            .collect();
        out.push(Capability {
            file,
            windows,
            commands,
        });
    }
    out
}

/// App commands reachable from `window`, unioned across every capability bound
/// to it — a command is reachable from a window if *any* capability that lists
/// the window grants it.
fn commands_for_window(window: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for cap in capabilities() {
        if cap.windows.iter().any(|w| w == window) {
            out.extend(cap.commands);
        }
    }
    out
}

/// String literals in `s`, assuming no escaped quotes (command names have none):
/// splitting on `"` puts every literal's body at an odd index.
fn string_literals(s: &str) -> BTreeSet<String> {
    s.split('"').skip(1).step_by(2).map(str::to_owned).collect()
}

#[test]
fn manifest_and_handler_list_the_same_commands() {
    let manifest = build_rs_commands();
    let handler = generate_handler_commands();
    assert_eq!(
        manifest,
        handler,
        "build.rs `.commands(&[…])` and lib.rs `generate_handler![…]` have drifted.\n  \
         only in build.rs: {:?}\n  only in generate_handler!: {:?}\n  \
         (a handler-only command has no generated permission, so every webview invocation is rejected)",
        manifest.difference(&handler).collect::<Vec<_>>(),
        handler.difference(&manifest).collect::<Vec<_>>(),
    );
}

#[test]
fn every_granted_permission_maps_to_a_registered_command() {
    let registered = build_rs_commands();
    for cap in capabilities() {
        for cmd in &cap.commands {
            assert!(
                registered.contains(cmd),
                "{} grants `allow-{}` but no such command is registered — a typo or stale \
                 entry that the Tauri runtime would reject silently",
                cap.file,
                cmd.replace('_', "-"),
            );
        }
    }
}

#[test]
fn palette_and_settings_share_only_audited_read_only_commands() {
    let palette = commands_for_window("main");
    let settings = commands_for_window("settings");
    let shared: BTreeSet<String> = palette.intersection(&settings).cloned().collect();
    // The only commands intentionally reachable from both webviews: read-only
    // stores with no side effect. Anything else shared widens the surface a
    // compromise of one window can reach into the other. If the split changes
    // on purpose, update this set *and* docs/command-surface.md together.
    let expected: BTreeSet<String> = [
        "get_settings",
        "get_permissions",
        "get_capabilities",
        "last_hotkey_failure",
        "get_ai_availability",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        shared, expected,
        "palette.json and settings.json may share only the audited read-only commands",
    );
}

#[test]
fn palette_cannot_invoke_privileged_settings_commands() {
    let palette = commands_for_window("main");
    // Commands that persist settings, run the installer/updater, drive an OS
    // permission prompt, or hard-delete data. The settings webview owns these;
    // a compromised palette webview must never reach them — even via a grant in
    // the shared `default.json` (which binds to the palette window too).
    for cmd in [
        "update_settings",
        "purge_deleted_entries",
        "request_accessibility",
        "rebuild_semantic_index",
        "check_for_updates",
        "install_cli",
    ] {
        assert!(
            !palette.contains(cmd),
            "palette.json must not grant `allow-{}` — it is a settings/installer command",
            cmd.replace('_', "-"),
        );
    }
}

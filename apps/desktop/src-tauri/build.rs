fn main() {
    // On macOS the app links the Apple AI bridge transitively (via
    // `nagori-platform-native` → `nagori-ai-apple`), whose Swift static library
    // references `@rpath/libswift_Concurrency.dylib`. A dependency build
    // script's `rustc-link-arg` does not propagate to the final binary, so add
    // the system Swift library directory as a runpath here.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }

    // Enumerate every app command so tauri-build autogenerates an
    // `allow-<command>` / `deny-<command>` permission for each. Declaring the
    // manifest flips app commands from "allowed in every window by default" to
    // deny-by-default: a command invoked from a webview is rejected unless a
    // capability for that window explicitly allows it. The per-window grants
    // live in `capabilities/palette.json` and `capabilities/settings.json`, so
    // a compromised palette webview cannot reach `clear_history`, `install_cli`,
    // `update_settings`, or the other privileged settings/installer commands —
    // and the settings webview cannot drive paste/copy/delete. Commands omitted
    // from every capability (e.g. `clear_history`, `add_entry`, `repaste_last`)
    // are never invoked from a webview today and stay unreachable from one.
    //
    // The list must stay in sync with `generate_handler!` in `lib.rs`: a command
    // registered there but missing here has no generated permission, so no
    // capability can grant it and any webview invocation is rejected.
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "search_clipboard",
            "list_recent_entries",
            "list_pinned_entries",
            "get_entry",
            "copy_entry",
            "paste_entry",
            "open_palette",
            "close_palette",
            "paste_entry_from_palette",
            "paste_entry_representation_from_palette",
            "list_paste_options",
            "copy_entry_from_palette",
            "get_entry_preview",
            "get_entry_preview_full",
            "preview_entry",
            "add_entry",
            "delete_entry",
            "delete_entries",
            "purge_deleted_entries",
            "copy_entries_combined",
            "clear_history",
            "repaste_last",
            "pin_entry",
            "run_quick_action",
            "start_ai_action",
            "cancel_ai_action",
            "get_ai_availability",
            "get_semantic_index_status",
            "rebuild_semantic_index",
            "save_ai_result",
            "get_settings",
            "password_manager_preset",
            "update_settings",
            "set_capture_enabled",
            "get_permissions",
            "get_capabilities",
            "last_hotkey_failure",
            "request_accessibility",
            "open_url_external",
            "toggle_palette",
            "hide_palette",
            "open_settings",
            "close_settings",
            "check_for_updates",
            "cli_install_status",
            "install_cli",
        ]));
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}

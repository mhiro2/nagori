//! The manual "Check for updates" probe surfaced in Settings → Advanced.

use tauri::AppHandle;

use crate::error::{CommandError, CommandResult};

/// Manual "Check for updates now" probe surfaced in Settings → Advanced.
///
/// Returns the discovered release version when an update is available,
/// `None` when the bundled build is already current, and a friendly
/// error otherwise (network down, signature mismatch, malformed
/// updater JSON). MVP behaviour is read-only — we expose the
/// *availability* and the frontend renders the GitHub release link;
/// `download_and_install` is intentionally not wired up yet, so we
/// never silently install.
///
/// Runs on every OS: `release.yaml` ships signed bundles for macOS
/// (`.app`/`.dmg`), Windows (NSIS) and Linux (`deb` + `AppImage`),
/// and `latest.json` lists them all. Whether the discovered update
/// can be installed in place depends on the install medium —
/// reported via `UpdateInfoDto::download_supported` so the UI can
/// fall back to the GitHub release link when self-replacement is
/// not possible (e.g. a `deb`-installed binary, where dpkg would
/// need root).
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> CommandResult<Option<UpdateInfoDto>> {
    use tauri_plugin_updater::UpdaterExt;

    let updater = app
        .updater()
        .map_err(|err| CommandError::internal(format!("updater unavailable: {err}")))?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(UpdateInfoDto {
            version: update.version.clone(),
            current_version: update.current_version.clone(),
            release_notes: update.body,
            download_supported: in_place_update_supported(),
        })),
        Ok(None) => Ok(None),
        Err(err) => Err(CommandError::internal(format!(
            "update check failed: {err}"
        ))),
    }
}

/// Whether the bundle running on the current host can be replaced in
/// place by `update.download_and_install()`.
///
/// Delegates to `tauri::utils::platform::bundle_type()` — the same
/// signal `tauri-plugin-updater` itself uses to pick a manifest entry,
/// so the UI advertisement and the updater's actual in-place path
/// stay in lock-step. `.app` / `.dmg` and the Windows NSIS bundle run
/// as the user that launched them and can rewrite the install root
/// without a privilege prompt; `AppImage` behaves the same. `deb`
/// installs land under `/usr` and would need `dpkg` + root to
/// replace, so the UI links to the GitHub release instead. When the
/// bundle type is unknown (e.g. `cargo run` during development), the
/// safe default is "no in-place update".
fn in_place_update_supported() -> bool {
    use tauri::utils::{config::BundleType, platform::bundle_type};
    matches!(
        bundle_type(),
        Some(BundleType::App | BundleType::Dmg | BundleType::Nsis | BundleType::AppImage),
    )
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfoDto {
    pub version: String,
    pub current_version: String,
    pub release_notes: Option<String>,
    pub download_supported: bool,
}

//! The manual "Check for updates" probe and the user-confirmed in-place
//! install surfaced in Settings → Advanced.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter};

use crate::error::{CommandError, CommandResult};

/// Manual "Check for updates now" probe surfaced in Settings → Advanced.
///
/// Returns the discovered release version when an update is available,
/// `None` when the bundled build is already current, and a friendly
/// error otherwise (network down, signature mismatch, malformed
/// updater JSON). The probe itself never installs anything — that only
/// happens through `install_update`, which the user triggers explicitly.
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

/// Event carrying `install_update` progress to the settings webview.
/// Keep in lockstep with `TAURI_EVENTS.updateProgress` in
/// `src/app/lib/tauri.ts`.
pub(crate) const UPDATE_PROGRESS_EVENT: &str = "nagori://update_progress";

/// Set while an `install_update` run is in flight so a double click (or a
/// second settings invoke) cannot start a parallel download and install.
static UPDATE_INSTALL_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Minimum byte step between progress events when the server sends no
/// `Content-Length`, so the webview is not flooded with one event per chunk.
const UNKNOWN_TOTAL_PROGRESS_STEP: u64 = 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
enum UpdateProgressDto {
    #[serde(rename_all = "camelCase")]
    Downloading {
        downloaded: u64,
        total: Option<u64>,
    },
    Installing,
}

/// Download, verify, and install the update the user confirmed in Settings,
/// then restart into it.
///
/// `version` is the version the user saw next to the button. The feed is
/// re-checked here and the run is refused when it now offers something
/// else, so the user never installs a release they did not confirm. The
/// plugin verifies the bundle against the `plugins.updater.pubkey`
/// minisign key before installing.
///
/// Shutdown goes through the normal quit cleanup on every OS: macOS and
/// `AppImage` swap the bundle in place and `request_restart` raises
/// `RunEvent::ExitRequested`, while the Windows NSIS path launches the
/// installer and calls `std::process::exit` itself, so the cleanup is
/// wired through `on_before_exit` there. The install runs on a blocking
/// thread so that hook can `block_on` the cleanup without nesting inside
/// the async runtime.
#[tauri::command]
pub async fn install_update(app: AppHandle, version: String) -> CommandResult<()> {
    if !in_place_update_supported() {
        return Err(CommandError::unsupported(
            "this install cannot be updated in place; download the new build from GitHub",
        ));
    }
    if UPDATE_INSTALL_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return Err(CommandError::invalid_input(
            "an update is already being installed",
        ));
    }
    let result = install_update_inner(&app, version.trim()).await;
    if let Err(err) = &result {
        // The webview only shows a generic sentence for `internal_error`, so
        // keep the verbatim cause (feed URL, signature failure) in the log.
        tracing::warn!(code = %err.code, message = %err.message, "update_install_failed");
        UPDATE_INSTALL_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
    result?;
    app.request_restart();
    Ok(())
}

async fn install_update_inner(app: &AppHandle, version: &str) -> CommandResult<()> {
    use tauri_plugin_updater::UpdaterExt;

    let cleanup_handle = app.clone();
    let updater = app
        .updater_builder()
        .on_before_exit(move || crate::perform_exit_cleanup(&cleanup_handle))
        .build()
        .map_err(|err| CommandError::internal(format!("updater unavailable: {err}")))?;
    let update = updater
        .check()
        .await
        .map_err(|err| CommandError::internal(format!("update check failed: {err}")))?
        .ok_or_else(|| CommandError::invalid_input("no update is available any more"))?;
    if update.version != version {
        return Err(CommandError::invalid_input(format!(
            "the available update changed to {}; check for updates again",
            update.version
        )));
    }

    let mut progress = DownloadProgress::default();
    let bytes = update
        .download(
            |chunk, total| {
                if let Some(event) = progress.advance(chunk, total) {
                    let _ = app.emit_to("settings", UPDATE_PROGRESS_EVENT, event);
                }
            },
            || {},
        )
        .await
        .map_err(|err| CommandError::internal(format!("update download failed: {err}")))?;
    let _ = app.emit_to(
        "settings",
        UPDATE_PROGRESS_EVENT,
        UpdateProgressDto::Installing,
    );
    tauri::async_runtime::spawn_blocking(move || update.install(bytes))
        .await
        .map_err(|err| CommandError::internal(format!("the install task did not complete: {err}")))?
        .map_err(|err| CommandError::internal(format!("update install failed: {err}")))
}

/// Throttles the per-chunk download callback down to an event per whole
/// percent (or per `UNKNOWN_TOTAL_PROGRESS_STEP` without a known total).
#[derive(Debug, Default)]
struct DownloadProgress {
    downloaded: u64,
    last_reported: Option<u64>,
}

impl DownloadProgress {
    fn advance(&mut self, chunk: usize, total: Option<u64>) -> Option<UpdateProgressDto> {
        self.downloaded = self.downloaded.saturating_add(chunk as u64);
        let mark = match total {
            Some(total) if total > 0 => self.downloaded.saturating_mul(100) / total,
            _ => self.downloaded / UNKNOWN_TOTAL_PROGRESS_STEP,
        };
        if self.last_reported == Some(mark) {
            return None;
        }
        self.last_reported = Some(mark);
        Some(UpdateProgressDto::Downloading {
            downloaded: self.downloaded,
            total,
        })
    }
}

/// GitHub release page the "View release" link in Settings → Advanced
/// points at. The tag prefix matches `release.yaml`'s `v*` trigger.
const RELEASE_PAGE_URL_PREFIX: &str = "https://github.com/mhiro2/nagori/releases/tag/v";

/// Open the GitHub release page for `version` in the default browser.
///
/// The settings webview cannot open it with a plain `<a target="_blank">`:
/// the Tauri webview drops new-window requests unless a handler is
/// registered, so the click was a silent no-op. Routing through a command
/// reuses the same platform opener as `open_url_external`. The renderer
/// only sends the version string — the URL is assembled here so a
/// compromised settings webview cannot point the opener at an arbitrary
/// target.
#[tauri::command]
pub async fn open_release_page(version: String) -> CommandResult<()> {
    let url = release_page_url(&version)?;
    crate::commands::settings_commands::open_external_url(url).await
}

/// Build the release page URL for `version`, rejecting anything that does
/// not look like a semver version (digits first, then only the
/// characters semver allows in pre-release / build metadata) so the
/// value cannot add path segments, a query, or a fragment.
fn release_page_url(version: &str) -> Result<String, CommandError> {
    let version = version.trim();
    let valid = version.len() <= 64
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if !valid {
        return Err(CommandError::invalid_input(format!(
            "invalid release version `{version}`"
        )));
    }
    Ok(format!("{RELEASE_PAGE_URL_PREFIX}{version}"))
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
pub(crate) fn in_place_update_supported() -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_progress_reports_each_percent_once() {
        let mut progress = DownloadProgress::default();
        assert!(progress.advance(1, Some(1000)).is_some());
        // 0.1% → 0.5% stays inside the first percent.
        assert!(progress.advance(4, Some(1000)).is_none());
        let Some(UpdateProgressDto::Downloading { downloaded, total }) =
            progress.advance(5, Some(1000))
        else {
            panic!("crossing into 1% should report");
        };
        assert_eq!((downloaded, total), (10, Some(1000)));
        assert!(progress.advance(990, Some(1000)).is_some());
    }

    #[test]
    fn download_progress_steps_by_bytes_without_a_total() {
        let mut progress = DownloadProgress::default();
        assert!(progress.advance(10, None).is_some());
        assert!(progress.advance(1024, None).is_none());
        let step = usize::try_from(UNKNOWN_TOTAL_PROGRESS_STEP).unwrap();
        assert!(progress.advance(step, None).is_some());
    }

    #[test]
    fn update_progress_serializes_with_a_phase_tag() {
        let downloading = serde_json::to_value(UpdateProgressDto::Downloading {
            downloaded: 5,
            total: None,
        })
        .unwrap();
        assert_eq!(
            downloading,
            serde_json::json!({ "phase": "downloading", "downloaded": 5, "total": null })
        );
        assert_eq!(
            serde_json::to_value(UpdateProgressDto::Installing).unwrap(),
            serde_json::json!({ "phase": "installing" })
        );
    }

    #[test]
    fn release_page_url_accepts_semver_versions() {
        assert_eq!(
            release_page_url("0.2.0").unwrap(),
            "https://github.com/mhiro2/nagori/releases/tag/v0.2.0"
        );
        assert_eq!(
            release_page_url(" 1.0.0-rc.1+build.5 ").unwrap(),
            "https://github.com/mhiro2/nagori/releases/tag/v1.0.0-rc.1+build.5"
        );
    }

    #[test]
    fn release_page_url_rejects_non_version_input() {
        for version in [
            "",
            "v0.2.0",
            "0.2.0/../../evil",
            "0.2.0?x=1",
            "0.2.0#frag",
            "0.2.0 extra",
            "https://example.com",
        ] {
            assert!(
                release_page_url(version).is_err(),
                "`{version}` should be rejected"
            );
        }
        assert!(release_page_url(&"1".repeat(65)).is_err());
    }
}

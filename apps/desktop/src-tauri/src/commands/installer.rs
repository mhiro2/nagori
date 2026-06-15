//! The Settings → CLI one-click installer: locating the bundled `nagori`
//! sidecar, probing the user's shell `PATH`, and symlinking it into
//! `~/.local/bin` (Unix only; Windows shows manual guidance).

use std::path::{Path, PathBuf};

use crate::dto::{CliInstallResultDto, CliInstallStatusDto};
use crate::error::{CommandError, CommandResult};

/// Name of the bundled CLI binary as it ships beside the desktop executable.
/// Tauri strips the target triple from the `bundle.externalBin` entry when it
/// copies the sidecar into the app, so the on-disk name is just `nagori`
/// (`nagori.exe` on Windows).
#[cfg(windows)]
const BUNDLED_CLI_NAME: &str = "nagori.exe";
#[cfg(not(windows))]
const BUNDLED_CLI_NAME: &str = "nagori";

/// Absolute path to the bundled `nagori` CLI that rides next to the desktop
/// executable (declared via `bundle.externalBin`), or `None` when it is
/// missing — most notably under `tauri dev`, where sidecars are not staged
/// beside the dev binary.
fn bundled_cli_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join(BUNDLED_CLI_NAME);
    candidate.is_file().then_some(candidate)
}

/// Per-user `bin` directory the in-app installer links into. `~/.local/bin`
/// is writable without elevation; the user may still need to add it to `PATH`
/// (surfaced via `on_path`).
fn cli_bin_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".local").join("bin"))
}

/// Canonicalise `path`, falling back to the path itself when it cannot be
/// resolved (e.g. a dangling link) so comparisons stay total.
fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The directories on the user's *shell* `PATH`. A GUI app launched from
/// Finder inherits launchd's minimal `PATH`, not the login shell's, so we ask
/// the login+interactive shell for its `PATH` and fall back to the process
/// environment only when that probe fails. Resolved once per status probe so
/// the (possibly slow) shell spawn happens at most once.
fn shell_path_dirs() -> Vec<PathBuf> {
    let raw = user_shell_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    std::env::split_paths(&raw).collect()
}

/// Whether `dir` appears among `path_dirs` (compared canonically).
fn dir_in(dir: &Path, path_dirs: &[PathBuf]) -> bool {
    let target = canonical_or_self(dir);
    path_dirs
        .iter()
        .any(|entry| canonical_or_self(entry) == target)
}

/// Best-effort check that `dir` is on the user's shell `PATH`.
#[cfg(unix)]
fn dir_on_path(dir: &Path) -> bool {
    dir_in(dir, &shell_path_dirs())
}

/// Find an already-installed `nagori` that resolves to `source`. Considers the
/// in-app installer's own `~/.local/bin` target and every directory on the
/// user's shell `PATH` — so a Homebrew-cask link (which lives in Homebrew's
/// `bin`, not `~/.local/bin`) counts as installed and the UI does not nag the
/// user to create a redundant second link. A match whose directory is on
/// `PATH` is preferred over one that is not, so `on_path` (derived from the
/// returned link) reflects whether `nagori` is actually reachable.
fn find_linked_cli(
    source: &Path,
    bin_dir: Option<&Path>,
    path_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let source = std::fs::canonicalize(source).ok()?;
    let candidates: Vec<PathBuf> = bin_dir
        .map(|dir| dir.join("nagori"))
        .into_iter()
        .chain(path_dirs.iter().map(|dir| dir.join(BUNDLED_CLI_NAME)))
        .filter(|cand| std::fs::canonicalize(cand).is_ok_and(|resolved| resolved == source))
        .collect();
    candidates
        .iter()
        .find(|cand| cand.parent().is_some_and(|dir| dir_in(dir, path_dirs)))
        .or_else(|| candidates.first())
        .cloned()
}

/// Whether an existing `~/.local/bin/nagori` entry is a symlink Nagori
/// plausibly created and may therefore repoint, versus a regular file or a
/// foreign symlink the user placed themselves (which must never be clobbered).
/// Three shapes count as ours:
///   * it already resolves to the current bundled CLI (a live, exact match) —
///     the common idempotent re-run;
///   * its target carries the macOS app-bundle shape
///     (`…/Nagori.app/Contents/MacOS/nagori`), so a still-present copy at an
///     older app location counts even when it no longer resolves to `source`;
///   * it is dangling (resolves to nothing) and its final component is the
///     bundled CLI name — an old Nagori link whose former app location is
///     gone. A dangling link is already broken, so repointing it is a repair,
///     not a clobber; this is what lets a prior *Linux* install (whose bundle
///     path is not the macOS shape) be repointed instead of refused as
///     foreign. A *live* foreign link still fails the check and is preserved.
#[cfg(unix)]
fn is_repointable_link(meta: &std::fs::Metadata, dest: &Path, source_canonical: &Path) -> bool {
    if !meta.file_type().is_symlink() {
        return false;
    }
    if canonical_or_self(dest).as_path() == source_canonical {
        return true;
    }
    let Ok(target) = std::fs::read_link(dest) else {
        return false;
    };
    if target.ends_with("Nagori.app/Contents/MacOS/nagori") {
        return true;
    }
    // Dangling link whose final component is the bundled CLI name. `metadata`
    // follows the link; only a NotFound means the target is genuinely gone, so
    // a permission (or other) error is treated as "not dangling" — an
    // unreadable foreign link must never be repointed.
    let dangling = matches!(
        std::fs::metadata(dest),
        Err(ref err) if err.kind() == std::io::ErrorKind::NotFound
    );
    dangling && target.file_name() == Some(std::ffi::OsStr::new(BUNDLED_CLI_NAME))
}

/// Whether the bundled binary lives somewhere stable enough to symlink
/// against. macOS App Translocation and `.dmg`-mounted launches, and Linux
/// `AppImage` mounts, expose the executable from an ephemeral path that
/// vanishes when the app quits — a symlink into one of those would dangle. We
/// refuse to link in those cases rather than create a link that silently breaks.
#[cfg(unix)]
fn cli_source_is_stable(path: &Path) -> bool {
    let shown = path.to_string_lossy();
    // macOS: Gatekeeper runs quarantined apps from a randomised read-only
    // `AppTranslocation` copy; `/Volumes/...` means we're running from the
    // still-mounted disk image.
    if shown.contains("/AppTranslocation/") || shown.starts_with("/Volumes/") {
        return false;
    }
    // Linux AppImage: the runtime fuse-mounts the bundle under `/tmp/.mount_*`
    // (exported as `$APPDIR`).
    if shown.contains("/.mount_") {
        return false;
    }
    if std::env::var("APPDIR")
        .is_ok_and(|appdir| !appdir.is_empty() && shown.starts_with(appdir.as_str()))
    {
        return false;
    }
    true
}

/// Ask the user's login shell for its effective `PATH`. Runs
/// `$SHELL -lic 'printf %s "$PATH"'` so both login (`.zprofile`,
/// `.bash_profile`) and interactive (`.zshrc`, `.bashrc`) edits — where
/// `~/.local/bin` additions usually live — are reflected. `stdin` is closed so
/// an interactive shell never blocks on a read. Returns `None` on any failure
/// so the caller can fall back to the process `PATH`.
#[cfg(unix)]
fn user_shell_path() -> Option<String> {
    use std::io::Read;
    let shell = std::env::var("SHELL").ok()?;
    let mut child = std::process::Command::new(shell)
        .args(["-lic", r#"printf %s "$PATH""#])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // Bound the probe: a slow `.zshrc` / `.bashrc` (network calls, version
    // managers, prompts that wait on a subcommand) must not wedge the Settings
    // CLI tab. Poll for exit and kill the shell if it overruns, then fall back
    // to the process `PATH`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(Some(_)) | Err(_) => return None,
        }
    }
    let mut path = String::new();
    child.stdout.take()?.read_to_string(&mut path).ok()?;
    (!path.trim().is_empty()).then_some(path)
}

#[cfg(not(unix))]
fn user_shell_path() -> Option<String> {
    None
}

/// Report whether the bundled `nagori` CLI is reachable and whether it has
/// been linked onto the user's `PATH`. Drives the Settings → CLI install
/// affordance. `supported` is `false` on Windows, where the one-click
/// installer is not wired yet and the UI shows manual guidance instead.
///
/// Async because it shells out to the user's login shell to read `PATH`
/// (potentially slow); `spawn_blocking` keeps that work off the main thread so
/// the UI never freezes while the CLI tab loads.
#[tauri::command]
pub async fn cli_install_status() -> CliInstallStatusDto {
    tauri::async_runtime::spawn_blocking(cli_install_status_blocking)
        .await
        .unwrap_or_else(|_| CliInstallStatusDto {
            supported: cfg!(unix),
            bundled: false,
            installed: false,
            installed_path: String::new(),
            bin_dir: String::new(),
            on_path: false,
        })
}

fn cli_install_status_blocking() -> CliInstallStatusDto {
    let bundled = bundled_cli_path();
    let bin_dir = cli_bin_dir();
    let path_dirs = shell_path_dirs();
    // An existing link counts whether the user installed via this button
    // (`~/.local/bin`) or via the Homebrew cask (Homebrew's bin), so the UI
    // does not show "not installed" to cask users.
    let linked = bundled
        .as_deref()
        .and_then(|source| find_linked_cli(source, bin_dir.as_deref(), &path_dirs));
    // When linked, report whether *that* link is reachable (its directory on
    // PATH) — a Homebrew link lives outside `~/.local/bin`. When not linked,
    // fall back to whether the install target dir is on PATH so the pre-install
    // UI can warn up front.
    let on_path = match linked.as_deref().and_then(Path::parent) {
        Some(dir) => dir_in(dir, &path_dirs),
        None => bin_dir
            .as_deref()
            .is_some_and(|dir| dir_in(dir, &path_dirs)),
    };
    // Report the actual link location when found; otherwise the path this
    // installer *would* use, so the UI can name it before installing.
    let installed_path = linked
        .clone()
        .or_else(|| bin_dir.as_ref().map(|dir| dir.join("nagori")))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    CliInstallStatusDto {
        supported: cfg!(unix),
        bundled: bundled.is_some(),
        installed: linked.is_some(),
        installed_path,
        bin_dir: bin_dir
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default(),
        on_path,
    }
}

/// Symlink the bundled `nagori` CLI into `~/.local/bin` so it is callable from
/// a terminal. Idempotent: re-running repoints an existing link (e.g. after
/// the app moves). Returns where the link landed and whether the directory is
/// on `PATH` so the UI can prompt the user to extend it.
///
/// macOS / Linux only — `~/.local/bin` is user-writable, so no elevation is
/// needed. The link targets the binary inside the installed app bundle, so it
/// keeps working across in-place updates that replace the app at the same
/// path.
///
/// Async because the link work shells out to the user's login shell to read
/// `PATH` (a slow `.zshrc` can take up to the 2 s probe deadline); like
/// `cli_install_status`, `spawn_blocking` keeps that off the main thread so the
/// Settings UI never freezes on the button press.
#[cfg(unix)]
#[tauri::command]
pub async fn install_cli() -> CommandResult<CliInstallResultDto> {
    tauri::async_runtime::spawn_blocking(install_cli_blocking)
        .await
        .map_err(|err| CommandError::internal(format!("CLI install task failed: {err}")))?
}

#[cfg(unix)]
fn install_cli_blocking() -> CommandResult<CliInstallResultDto> {
    let source = bundled_cli_path().ok_or_else(|| {
        CommandError::internal(
            "the bundled nagori CLI was not found beside the app — install the packaged \
             app first (this action is unavailable under `tauri dev`)",
        )
    })?;
    // Refuse to link against an ephemeral copy of the app — the link would
    // dangle once the disk image is ejected or the translocated copy is reaped.
    if !cli_source_is_stable(&source) {
        return Err(CommandError::invalid_input(
            "Nagori is running from a temporary location (a disk image or a translocated \
             copy). Move Nagori to your Applications folder and relaunch it, then install \
             the CLI.",
        ));
    }
    let source_canonical = canonical_or_self(&source);
    let bin_dir = cli_bin_dir()
        .ok_or_else(|| CommandError::internal("could not resolve the home directory"))?;
    std::fs::create_dir_all(&bin_dir).map_err(|err| {
        CommandError::internal(format!("failed to create {}: {err}", bin_dir.display()))
    })?;
    let dest = bin_dir.join("nagori");
    // Idempotently repoint a link we created (handles the app moving between
    // versions), but never clobber a regular file or a foreign symlink the
    // user placed there themselves.
    match std::fs::symlink_metadata(&dest) {
        Ok(meta) => {
            if !is_repointable_link(&meta, &dest, &source_canonical) {
                return Err(CommandError::invalid_input(format!(
                    "{} already exists and was not created by Nagori. Remove it manually \
                     and retry.",
                    dest.display()
                )));
            }
            std::fs::remove_file(&dest).map_err(|err| {
                CommandError::internal(format!("failed to replace {}: {err}", dest.display()))
            })?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(CommandError::internal(format!(
                "failed to inspect {}: {err}",
                dest.display()
            )));
        }
    }
    // `symlink` refuses to overwrite an existing path, so if a regular file or
    // a foreign link races into `dest` between the check above and here it
    // errors rather than clobbering it — preserving the "never clobber what
    // isn't ours" invariant. (An atomic rename-over-dest would replace the
    // entry unconditionally and lose that guarantee, so we keep the
    // remove-then-create shape.)
    std::os::unix::fs::symlink(&source, &dest).map_err(|err| {
        CommandError::internal(format!(
            "failed to link {} -> {}: {err}",
            dest.display(),
            source.display()
        ))
    })?;
    Ok(CliInstallResultDto {
        installed_path: dest.to_string_lossy().into_owned(),
        bin_dir: bin_dir.to_string_lossy().into_owned(),
        source_path: source.to_string_lossy().into_owned(),
        on_path: dir_on_path(&bin_dir),
    })
}

#[cfg(not(unix))]
#[tauri::command]
pub fn install_cli() -> CommandResult<CliInstallResultDto> {
    Err(CommandError::unsupported("install_cli"))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn cli_source_is_stable_rejects_ephemeral_app_locations() {
        // Gatekeeper translocation, mounted disk images, and AppImage fuse
        // mounts all expose the executable from a path that vanishes when the
        // app quits — a symlink into one would dangle, so linking is refused.
        assert!(!cli_source_is_stable(Path::new(
            "/private/var/folders/AppTranslocation/abc/d/Nagori.app/Contents/MacOS/nagori"
        )));
        assert!(!cli_source_is_stable(Path::new(
            "/Volumes/Nagori/Nagori.app/Contents/MacOS/nagori"
        )));
        assert!(!cli_source_is_stable(Path::new(
            "/tmp/.mount_Nagoriabc/usr/bin/nagori"
        )));

        // A normal install location is stable. Guard the positive case on
        // APPDIR being unset so a test host that happens to export it (an
        // AppImage CI runner) does not flip the result.
        if std::env::var_os("APPDIR").is_none() {
            assert!(cli_source_is_stable(Path::new(
                "/Applications/Nagori.app/Contents/MacOS/nagori"
            )));
        }
    }

    #[test]
    fn dir_in_matches_through_a_symlinked_alias() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real_bin");
        std::fs::create_dir(&real).expect("create dir");
        let alias = tmp.path().join("alias_bin");
        symlink(&real, &alias).expect("symlink dir");

        // The alias resolves to the same directory, so a PATH listing either
        // form must count as a match.
        assert!(dir_in(&alias, std::slice::from_ref(&real)));
        assert!(dir_in(&real, std::slice::from_ref(&alias)));
        assert!(!dir_in(&real, &[tmp.path().join("unrelated")]));
    }

    #[test]
    fn find_linked_cli_prefers_a_link_that_is_on_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source_nagori");
        std::fs::write(&source, b"#!/bin/sh\n").expect("write source");

        // The installer's own ~/.local/bin target (not on PATH) and a
        // PATH directory both link to the same source.
        let local_bin = tmp.path().join("local_bin");
        std::fs::create_dir(&local_bin).expect("create local_bin");
        symlink(&source, local_bin.join("nagori")).expect("symlink local");

        let path_bin = tmp.path().join("path_bin");
        std::fs::create_dir(&path_bin).expect("create path_bin");
        symlink(&source, path_bin.join("nagori")).expect("symlink path");

        let found = find_linked_cli(&source, Some(&local_bin), std::slice::from_ref(&path_bin))
            .expect("an installed link is found");
        assert_eq!(
            found,
            path_bin.join("nagori"),
            "a link on PATH is preferred over the off-PATH ~/.local/bin link",
        );
    }

    #[test]
    fn is_repointable_link_repoints_a_dangling_nagori_link() {
        // A prior install's link whose former app location is gone: dangling,
        // final component `nagori`. Linux bundle paths don't match the macOS
        // app-bundle shape, so this branch is what lets a stale Linux link be
        // repointed instead of refused as foreign.
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("nagori");
        symlink(tmp.path().join("gone/usr/bin/nagori"), &dest).expect("dangling symlink");
        let meta = std::fs::symlink_metadata(&dest).expect("symlink meta");
        let source_canonical = tmp.path().join("new/nagori");
        assert!(is_repointable_link(&meta, &dest, &source_canonical));
    }

    #[test]
    fn is_repointable_link_refuses_a_live_foreign_link() {
        // A symlink the user created themselves to their own build: live
        // (target exists), not the macOS shape. Even though its filename is
        // `nagori`, a live foreign link must be preserved, not clobbered.
        let tmp = tempfile::tempdir().expect("tempdir");
        let foreign_target = tmp.path().join("custom-build").join("nagori");
        std::fs::create_dir_all(foreign_target.parent().unwrap()).expect("mkdir");
        std::fs::write(&foreign_target, b"#!/bin/sh\n").expect("write target");
        let dest = tmp.path().join("nagori");
        symlink(&foreign_target, &dest).expect("foreign symlink");
        let meta = std::fs::symlink_metadata(&dest).expect("symlink meta");
        let source_canonical = tmp.path().join("bundled/nagori");
        assert!(!is_repointable_link(&meta, &dest, &source_canonical));
    }

    #[test]
    fn is_repointable_link_repoints_a_live_link_to_the_current_source() {
        // The common idempotent re-run: the link already resolves to the
        // bundled CLI we're about to install.
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("nagori-cli");
        std::fs::write(&source, b"#!/bin/sh\n").expect("write source");
        let source_canonical = canonical_or_self(&source);
        let dest = tmp.path().join("nagori");
        symlink(&source, &dest).expect("symlink");
        let meta = std::fs::symlink_metadata(&dest).expect("symlink meta");
        assert!(is_repointable_link(&meta, &dest, &source_canonical));
    }

    #[test]
    fn is_repointable_link_refuses_a_regular_file() {
        // A real file (not a symlink) is never ours, even if named `nagori`.
        let tmp = tempfile::tempdir().expect("tempdir");
        let dest = tmp.path().join("nagori");
        std::fs::write(&dest, b"#!/bin/sh\n").expect("write file");
        let meta = std::fs::symlink_metadata(&dest).expect("meta");
        assert!(!is_repointable_link(&meta, &dest, &tmp.path().join("src")));
    }

    #[test]
    fn find_linked_cli_returns_none_when_no_link_resolves_to_the_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = tmp.path().join("source_nagori");
        std::fs::write(&source, b"#!/bin/sh\n").expect("write source");

        let empty_bin = tmp.path().join("empty_bin");
        std::fs::create_dir(&empty_bin).expect("create empty_bin");

        assert!(
            find_linked_cli(&source, Some(&empty_bin), &[]).is_none(),
            "no symlink points at the source, so nothing is reported installed",
        );
    }
}

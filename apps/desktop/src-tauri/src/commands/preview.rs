//! Entry preview commands: the inline preview DTO bodies and the OS-native
//! Quick Look overlay (macOS), plus the plaintext temp-file cache the
//! overlay materialises and its cleanup helpers.

use std::path::{Path, PathBuf};

use nagori_core::{AppError, EntryId, Sensitivity};
use nagori_daemon::NagoriRuntime;
use nagori_platform::PreviewItem;
use tauri::State;

use crate::dto::EntryPreviewDto;
use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

use super::parse_entry_id;

/// Build the standard 128 KiB preview body for an entry. The optional
/// `query` is best-effort: when truncation kicks in, the DTO flags
/// whether the query substring lives in the elided middle so the UI can
/// warn that a search hit is hidden. Empty / whitespace queries are
/// ignored so a pristine palette never sees a spurious warning.
#[tauri::command]
pub async fn get_entry_preview(
    state: State<'_, AppState>,
    entry_id: String,
    query: Option<String>,
) -> CommandResult<EntryPreviewDto> {
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // Fold the current user's home to `~` in file-list locations. Display-only
    // shortening, never a privacy boundary — the body is already gated to
    // non-sensitive entries before any raw path is surfaced.
    let home = dirs::home_dir().map(|path| path.to_string_lossy().into_owned());
    Ok(EntryPreviewDto::from_entry_with_query(
        &entry,
        query.as_deref(),
        home.as_deref(),
    ))
}

/// Larger 1 MiB preview body for the expanded preview pane. Sensitivity
/// gating mirrors `get_entry_preview` but is enforced explicitly: any
/// entry that is not `Public` is rejected with a `forbidden` code so the
/// frontend can render a curated message rather than re-fetching with
/// the standard cap.
#[tauri::command]
pub async fn get_entry_preview_full(
    state: State<'_, AppState>,
    entry_id: String,
) -> CommandResult<EntryPreviewDto> {
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // Only Public bodies may flow through the full-content path. The
    // standard preview already redacts Private / Secret / Blocked bodies
    // to a placeholder, but the expanded pane hands the entire window
    // over to the body, so the gate is enforced explicitly here.
    if !matches!(entry.sensitivity, Sensitivity::Public) {
        return Err(CommandError::forbidden(
            "expanded preview is only available for Public entries",
        ));
    }
    let home = dirs::home_dir().map(|path| path.to_string_lossy().into_owned());
    Ok(EntryPreviewDto::from_entry_full(&entry, home.as_deref()))
}
/// Open an OS-native preview overlay for the entry (Quick Look on
/// macOS).
///
/// The palette binds Cmd+Y to this command. The body is gated to
/// `Public` entries because Quick Look materialises content into a
/// temp file readable by any process running as the user — surfacing
/// a `Private` / `Secret` body through the cross-process preview path
/// would silently undo the sensitivity classifier's job. `Blocked`
/// content can never reach this command because it is dropped at
/// capture time.
///
/// `FileList` entries hand the stored paths to the preview API
/// directly; image entries write the payload bytes to a temp file
/// keyed on the entry id; text-flavoured entries render through a
/// `.txt` temp file so Quick Look's text preview can syntax-highlight.
/// Empty / unrecoverable payloads return `InvalidInput` so the palette
/// can fall back to its in-line preview pane.
///
/// On Windows / Linux the preview controller is the
/// [`UnsupportedPreviewController`] stub. The command short-circuits
/// on the capability row *before* materialising any temp file so a
/// forged invoke from a non-macOS host cannot leave preview artefacts
/// in `/tmp` even though the call ultimately fails.
#[tauri::command]
pub async fn preview_entry(state: State<'_, AppState>, entry_id: String) -> CommandResult<()> {
    if !state.runtime.capabilities().preview_quick_look.is_usable() {
        return Err(
            AppError::Unsupported("preview is not available on this platform".to_owned()).into(),
        );
    }
    let entry_id = parse_entry_id(&entry_id)?;
    let entry = state
        .runtime
        .get_entry(entry_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !matches!(entry.sensitivity, Sensitivity::Public) {
        return Err(CommandError::forbidden(
            "preview is only available for Public entries",
        ));
    }
    let items: Vec<PreviewItem> = match &entry.content {
        nagori_core::ClipboardContent::FileList(file_list) => file_list
            .paths
            .iter()
            .map(|path| PreviewItem::new(PathBuf::from(path)))
            .collect(),
        nagori_core::ClipboardContent::Image(_) => {
            let Some((bytes, mime)) = state.runtime.get_payload(entry_id).await? else {
                return Err(CommandError::invalid_input(
                    "image payload is no longer stored",
                ));
            };
            let path = write_preview_temp_file(entry_id, &bytes, extension_for_image_mime(&mime))?;
            vec![PreviewItem::new(path)]
        }
        nagori_core::ClipboardContent::Text(_)
        | nagori_core::ClipboardContent::Url(_)
        | nagori_core::ClipboardContent::Code(_)
        | nagori_core::ClipboardContent::RichText(_)
        | nagori_core::ClipboardContent::Unknown(_) => {
            let Some(text) = entry.content.plain_text() else {
                return Err(CommandError::invalid_input(
                    "entry has no previewable plain text",
                ));
            };
            let path = write_preview_temp_file(entry_id, text.as_bytes(), "txt")?;
            vec![PreviewItem::new(path)]
        }
    };
    if items.is_empty() {
        return Err(CommandError::invalid_input(
            "entry has no content to preview",
        ));
    }
    // The entry may have been deleted elsewhere (CLI over IPC, retention)
    // while its body was being written out, after the orphan prune for that
    // delete had already scanned the directory. Re-check now that the file
    // exists, so a deleted body is not left behind in the temp directory.
    if state.runtime.get_entry(entry_id).await?.is_none() {
        remove_preview_temp_files_for(entry_id);
        return Err(AppError::NotFound.into());
    }
    state.preview.preview(&items).await?;
    Ok(())
}

/// Map a stored image mime to a Quick-Look-friendly extension. Falling
/// back to `png` is deliberate — every macOS Quick Look generator we
/// care about handles PNG, and the extension is only a hint (Quick
/// Look re-sniffs the bytes anyway).
fn extension_for_image_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/tiff" => "tiff",
        "image/bmp" => "bmp",
        "image/heic" | "image/heif" => "heic",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Directory holding the Quick Look preview temp files (`<entry_id>.<ext>`).
///
/// Lives under `std::env::temp_dir()` so it survives only as long as the OS
/// keeps `/tmp`; nagori additionally wipes it at startup and prunes it on
/// delete / clear so a previewed `Public` body never outlives its history row.
pub(crate) fn preview_temp_dir() -> PathBuf {
    std::env::temp_dir().join("nagori-preview")
}

/// Write `bytes` to `<preview_temp_dir>/<entry>.<ext>` and return the
/// path. The directory is created with the same private-mode helper the
/// `SQLite` store uses so the temp payload is not world-readable; reusing
/// the entry id as the filename means repeated previews of the same
/// entry overwrite a single file rather than littering the directory.
fn write_preview_temp_file(
    entry_id: EntryId,
    bytes: &[u8],
    extension: &str,
) -> Result<PathBuf, CommandError> {
    let dir = preview_temp_dir();
    nagori_storage::ensure_private_directory(&dir).map_err(|err| {
        CommandError::internal(format!(
            "could not prepare preview temp dir {}: {err}",
            dir.display()
        ))
    })?;
    let file = dir.join(format!("{entry_id}.{extension}"));
    std::fs::write(&file, bytes).map_err(|err| {
        CommandError::internal(format!(
            "could not write preview temp file {}: {err}",
            file.display()
        ))
    })?;
    Ok(file)
}

/// Remove the entire preview temp directory.
///
/// Called at startup (wipe the previous session's leftovers), from
/// `clear_history`, and from the `clear_on_quit` exit path so the palette's
/// "clear history" promise extends to the plaintext Quick Look cache. These
/// files are an ephemeral cache — a previewed entry regenerates its temp file
/// on the next `preview_entry` — so removal is purely a security-hygiene step
/// and is best-effort: a missing dir is a no-op and any IO error is logged at
/// debug rather than surfaced.
pub(crate) fn purge_preview_temp_dir() {
    purge_preview_temp_dir_in(&preview_temp_dir());
}

fn purge_preview_temp_dir_in(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::debug!(
            dir = %dir.display(),
            error = %err,
            "preview_temp_dir_purge_failed",
        ),
    }
}

/// Remove any preview temp file keyed on `entry_id` (`<entry_id>.<ext>`,
/// regardless of extension) so a deleted entry's previously-previewed body
/// does not linger in `/tmp`. Best-effort, matching [`purge_preview_temp_dir`]:
/// a missing directory / file is a no-op and IO errors are logged at debug.
pub(crate) fn remove_preview_temp_files_for(entry_id: EntryId) {
    remove_preview_temp_files_in(&preview_temp_dir(), entry_id);
}

/// Remove the preview temp files of entries that are no longer live.
///
/// The interactive delete / clear commands drop their entry's file directly,
/// but history also leaves through paths that never reach them: a CLI
/// `delete` / `clear` routed over IPC, and the retention sweep. Those only
/// surface as an IPC mutation or a finished maintenance run, so the desktop
/// reconciles the cache against the store at both points instead — any file
/// whose entry is gone (hard-deleted or tombstoned) is removed. Best-effort
/// like the other cleanup helpers: a store error keeps the file for the next
/// pass rather than guessing.
pub(crate) async fn prune_orphaned_preview_temp_files(runtime: &NagoriRuntime) {
    prune_orphaned_preview_temp_files_in(&preview_temp_dir(), runtime).await;
}

async fn prune_orphaned_preview_temp_files_in(dir: &Path, runtime: &NagoriRuntime) {
    for (entry_id, path) in preview_temp_files_in(dir) {
        match runtime.get_entry(entry_id).await {
            Ok(Some(_)) => {}
            Ok(None) => remove_preview_temp_file(&path),
            Err(err) => tracing::debug!(
                path = %path.display(),
                error = %err,
                "preview_temp_file_liveness_check_failed",
            ),
        }
    }
}

/// Every `<entry_id>.<ext>` file in the preview temp directory, paired with
/// the entry id its name carries. Files that do not follow that shape are not
/// ours to judge and are skipped.
fn preview_temp_files_in(dir: &Path) -> Vec<(EntryId, PathBuf)> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let entry_id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(|stem| stem.parse::<EntryId>().ok())?;
            Some((entry_id, path))
        })
        .collect()
}

fn remove_preview_temp_file(path: &Path) {
    if let Err(err) = std::fs::remove_file(path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(
            path = %path.display(),
            error = %err,
            "preview_temp_file_remove_failed",
        );
    }
}

fn remove_preview_temp_files_in(dir: &Path, entry_id: EntryId) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = entry_id.to_string();
    for entry in read_dir.flatten() {
        let path = entry.path();
        // Match exactly `<entry_id>.<ext>`: strip the id and require a literal
        // dot next, so id `…0001` cannot also sweep id `…00010`'s file.
        let matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(&prefix))
            .is_some_and(|rest| rest.starts_with('.'));
        if matches {
            remove_preview_temp_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir under the OS temp root so preview-cleanup tests
    /// never touch the real `nagori-preview/` dir or race each other.
    fn scratch_preview_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nagori-preview-test-{}", EntryId::new()));
        std::fs::create_dir_all(&dir).expect("create scratch preview dir");
        dir
    }

    #[test]
    fn remove_preview_temp_files_targets_only_the_entry_id() {
        let dir = scratch_preview_dir();
        let target = EntryId::new();
        let other = EntryId::new();
        // The target's image + text temp files, plus another entry's file and
        // a file whose name merely *starts with* the target id followed by a
        // non-dot character — that last one must survive (prefix-collision
        // guard).
        let target_png = dir.join(format!("{target}.png"));
        let target_txt = dir.join(format!("{target}.txt"));
        let other_txt = dir.join(format!("{other}.txt"));
        let look_alike = dir.join(format!("{target}-decoy.txt"));
        for path in [&target_png, &target_txt, &other_txt, &look_alike] {
            std::fs::write(path, b"x").expect("write fixture");
        }

        remove_preview_temp_files_in(&dir, target);

        assert!(
            !target_png.exists(),
            "target image temp file must be removed"
        );
        assert!(
            !target_txt.exists(),
            "target text temp file must be removed"
        );
        assert!(other_txt.exists(), "an unrelated entry's file must survive");
        assert!(
            look_alike.exists(),
            "a name that only shares the id prefix must not be swept",
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn purge_preview_temp_dir_removes_everything_and_tolerates_a_missing_dir() {
        let dir = scratch_preview_dir();
        std::fs::write(dir.join(format!("{}.txt", EntryId::new())), b"x").expect("write fixture");

        purge_preview_temp_dir_in(&dir);
        assert!(!dir.exists(), "purge must remove the whole preview dir");

        // Second call on the now-missing dir must be a silent no-op, matching
        // the best-effort contract (startup runs it before the dir exists).
        purge_preview_temp_dir_in(&dir);
    }

    #[tokio::test]
    async fn prune_removes_files_of_entries_deleted_outside_the_desktop() {
        use nagori_core::{EntryFactory, EntryRepository};

        let store = nagori_storage::SqliteStore::open_memory().expect("memory store");
        let runtime = NagoriRuntime::builder(store.clone()).build_for_test();
        let live = store
            .insert(EntryFactory::from_text("still in history"))
            .await
            .expect("insert");
        let tombstoned = store
            .insert(EntryFactory::from_text("deleted from the CLI"))
            .await
            .expect("insert");
        let purged = store
            .insert(EntryFactory::from_text("aged out by retention"))
            .await
            .expect("insert");
        store.mark_deleted(tombstoned).await.expect("tombstone");
        store.hard_delete_entry(purged).await.expect("hard delete");

        let dir = scratch_preview_dir();
        let live_file = dir.join(format!("{live}.txt"));
        let tombstoned_file = dir.join(format!("{tombstoned}.txt"));
        let purged_file = dir.join(format!("{purged}.png"));
        let foreign_file = dir.join("not-an-entry.txt");
        for path in [&live_file, &tombstoned_file, &purged_file, &foreign_file] {
            std::fs::write(path, b"x").expect("write fixture");
        }

        prune_orphaned_preview_temp_files_in(&dir, &runtime).await;

        assert!(live_file.exists(), "a live entry keeps its preview file");
        assert!(
            !tombstoned_file.exists(),
            "a deleted entry's file is pruned"
        );
        assert!(!purged_file.exists(), "a purged entry's file is pruned");
        assert!(
            foreign_file.exists(),
            "files that are not ours are left alone"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

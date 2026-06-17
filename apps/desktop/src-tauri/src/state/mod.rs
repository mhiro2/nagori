use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nagori_core::{AppError, EntryId, Result};
use nagori_daemon::NagoriRuntime;
use nagori_platform::{ClipboardReader, PreviewController, RestoreTarget, WindowBehavior};
use nagori_platform_native::{NativeRuntimeOptions, build_native_runtime};
use nagori_storage::SqliteStore;

mod clear_on_quit;
mod startup;
#[cfg(test)]
mod test_support;
mod window_focus;

use clear_on_quit::clear_on_quit_marker_path;
pub(crate) use startup::settings_loaded_or_shutdown;
use startup::{BackgroundTasks, SettingsLoadGate};

/// How long a "last pasted" entry id stays valid before it falls back to
/// the recency head. Picked at 30 min so a short break between pastes
/// (coffee, meeting) still threads back to the same clip, but a fresh
/// session many hours later doesn't surface a context-mismatched paste
/// from a different task.
const LAST_PASTED_TTL: Duration = Duration::from_mins(30);

pub struct AppState {
    pub runtime: NagoriRuntime,
    pub window: Arc<dyn WindowBehavior>,
    /// OS-native preview surface (Quick Look on macOS, `Unsupported`
    /// stub on Windows / Linux). The Tauri `preview_entry` command
    /// drives this directly from the desktop process rather than going
    /// through IPC — the daemon does not run an `AppKit` event loop, so
    /// the macOS adapter would not work from a free-standing daemon
    /// even if we wired it. The capability layer reports `Unsupported`
    /// on the OSes where this stub is wired, so palette UI can suppress
    /// the shortcut up front.
    pub preview: Arc<dyn PreviewController>,
    /// Clipboard reader handle shared with the runtime's writer. Holding the
    /// same `Arc` on both sides means the capture loop and the paste/copy path
    /// can't drift into a state where one is wired to a working adapter and
    /// the other to a stub: any platform-init failure surfaces once in
    /// `try_new_at` (with Wayland guidance on Linux) and aborts startup,
    /// rather than letting the app come up with the writer healthy and a
    /// silently-dead capture task.
    capture_reader: Arc<dyn ClipboardReader>,
    background_tasks: Mutex<Option<BackgroundTasks>>,
    /// Frontmost app captured the last time the palette was opened.
    /// Used by `paste_entry` to re-focus the user's prior app before
    /// synthesising ⌘V — without it, the keystroke lands on the
    /// (still-focused) Nagori webview and we paste into our own search
    /// box. On Linux Wayland the snapshot is always `None` (the
    /// compositor refuses to expose a portable frontmost-app query), so
    /// the palette skips the refocus step and relies on `wtype` to
    /// target whatever the compositor considers focused after our window
    /// hides. On Windows the snapshot now carries the foreground HWND
    /// in `native_handle`, so `activate_restore_target` can re-foreground
    /// the original window via `SetForegroundWindow`.
    pub previous_frontmost: Arc<Mutex<Option<RestoreTarget>>>,
    /// Most recently pasted entry id, paired with the `Instant` it was
    /// recorded. Powers the "repaste last" secondary hotkey so it
    /// targets the entry the user actually pasted instead of whatever
    /// happens to top the recency list (a fresh capture from elsewhere
    /// can otherwise hijack the slot between pastes). The timestamp
    /// drives TTL expiry — see `LAST_PASTED_TTL` — so a paste recorded
    /// hours ago doesn't silently resurface in a new working context.
    pub last_pasted_id: Mutex<Option<(EntryId, Instant)>>,
    /// Latest global-hotkey registration failures, split per `kind`.
    /// Persisted so the always-on App-level subscriber can re-hydrate
    /// the toast/banner after a window opens past the live emit (startup
    /// race) or after `SettingsView` is re-mounted later. A primary and
    /// a secondary failure are tracked independently so a primary
    /// success (or vice versa) doesn't silently wipe an unresolved
    /// failure on the other side. The frontend reads via
    /// `last_hotkey_failure` and subscribes to the live
    /// `nagori://hotkey_register_failed` / `_resolved` events for
    /// updates.
    pub last_hotkey_failure: Mutex<HotkeyFailureCache>,
    /// Single-instance lock over the data directory, held for the whole
    /// process lifetime. Two desktop instances (or a desktop instance plus a
    /// standalone daemon) owning the same store would double-capture the
    /// clipboard, race schema migrations, and let one process' clear-on-quit
    /// purge data the other still considers live. `try_new_at` acquires this
    /// before opening the store and refuses to start a second owner; `build`
    /// (used by tests with in-memory stores) leaves it `None`. The field is
    /// never read — its only job is to keep the OS lock alive until the
    /// process exits, at which point the kernel releases it.
    #[allow(dead_code)]
    instance_lock: Option<nagori_storage::ProcessLock>,
    /// Path to the clear-on-quit purge-pending marker. Set for real launches
    /// by `try_new_at`; `build`-only callers (tests / in-memory stores) own no
    /// on-disk directory and leave it `None`. When `clear_on_quit` is enabled,
    /// `perform_exit_cleanup` writes this sentinel *before* attempting the
    /// purge and removes it only once the purge completes within the shutdown
    /// budget. A launch that finds it present finishes the purge fail-closed
    /// before any window can serve history, so a timed-out / crashed shutdown
    /// purge can no longer leave behind data the user asked to clear.
    clear_on_quit_marker: Option<PathBuf>,
    /// Receiver side of the startup settings-load gate. Cloned by each gated
    /// worker (capture, CLI IPC host, settings subscriber) so they await the
    /// single coordinator load instead of each re-reading the store.
    settings_load_rx: tokio::sync::watch::Receiver<SettingsLoadGate>,
    /// Sender side, taken once by the coordinator in `spawn_background_tasks`.
    /// Held in a slot rather than moved into `build`'s return so the coordinator
    /// can publish the load outcome after the state is managed by Tauri.
    settings_load_tx: Mutex<Option<tokio::sync::watch::Sender<SettingsLoadGate>>>,
}

/// Snapshot of a global-hotkey registration failure shared across the
/// emit (`nagori://hotkey_register_failed`) and the cached state queried
/// by `last_hotkey_failure`. Kept in `state/mod.rs` so it can be referenced
/// from both `lib.rs` (emit site) and `commands/mod.rs` (query command)
/// without dragging the kind enum across module boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyFailureRecord {
    pub hotkey: String,
    pub error: String,
    /// `Some("secondary")` for secondary accelerators, `None` for the
    /// primary palette shortcut — mirrors the wire shape emitted on
    /// `nagori://hotkey_register_failed`.
    pub kind: Option<String>,
    /// Identifier of the secondary action whose register failed (the
    /// kebab-case wire value mirroring `SecondaryHotkeyAction`'s serde
    /// representation). `None` for primary failures, where the binding
    /// is global. Carried on the wire too: the frontend's single-slot
    /// store uses it to discriminate "this exact action resolved" from
    /// "a sibling action sharing the accelerator resolved", so a
    /// secondary resolve cannot wipe an unrelated still-failing
    /// secondary.
    pub action: Option<String>,
}

/// Cache of hotkey registration failures keyed for independent
/// resolution. The primary slot is single-slot (there is only one
/// palette shortcut), but secondaries are keyed by *action* — two
/// secondary actions can fail at the same time (or share the same
/// accelerator), and resolving one must not silently lose the other
/// from cache + hydration. The action key is the kebab-case wire value
/// (`repaste-last`, `clear-history`); cached secondary records without
/// an action identifier are skipped on insert because there would be
/// no way to address them on a later resolve.
#[derive(Debug, Default, Clone)]
pub struct HotkeyFailureCache {
    pub primary: Option<HotkeyFailureRecord>,
    pub secondary: BTreeMap<String, HotkeyFailureRecord>,
}

impl HotkeyFailureCache {
    /// Route a new failure to the matching slot. Primary records (and
    /// any record missing `kind`) land in the single primary slot; a
    /// secondary record is keyed by its `action` wire value. A
    /// secondary record without an action identifier is dropped — the
    /// per-action cache has no way to address it on a later resolve,
    /// so caching it would only produce permanently-stuck entries.
    pub fn record(&mut self, record: HotkeyFailureRecord) {
        match record.kind.as_deref() {
            Some("secondary") => {
                if let Some(action) = record.action.clone() {
                    self.secondary.insert(action, record);
                }
            }
            _ => self.primary = Some(record),
        }
    }

    /// Clear the slot identified by `kind` (+ `action` for secondaries).
    /// Returns whether anything was actually cleared so the caller can
    /// skip emitting a paired resolved event when the cache was empty.
    /// A secondary clear without an action is a no-op — a blanket clear
    /// would wipe sibling actions that share the kind, which is exactly
    /// the bug the per-action cache exists to prevent.
    pub fn clear_for_kind_action(&mut self, kind: Option<&str>, action: Option<&str>) -> bool {
        match (kind, action) {
            (None, _) => self.primary.take().is_some(),
            (Some("secondary"), Some(a)) => self.secondary.remove(a).is_some(),
            _ => false,
        }
    }

    /// Most-relevant cached failure for a single-slot consumer. Primary
    /// wins over secondary — the palette toggle being broken is
    /// strictly more disruptive than a missing secondary action. With
    /// no primary failure, returns an arbitrary-but-deterministic
    /// secondary (`BTreeMap` first entry — alphabetical by action wire
    /// value).
    pub fn most_relevant(&self) -> Option<&HotkeyFailureRecord> {
        self.primary
            .as_ref()
            .or_else(|| self.secondary.values().next())
    }
}

impl AppState {
    pub fn record_last_pasted(&self, id: EntryId) {
        let mut slot = self
            .last_pasted_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some((id, Instant::now()));
    }

    pub fn last_pasted(&self) -> Option<EntryId> {
        let mut slot = self
            .last_pasted_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (id, recorded_at) = (*slot)?;
        if recorded_at.elapsed() >= LAST_PASTED_TTL {
            // Expired entries are cleared on read so a stale id can't be
            // picked up by a later mutation path that compares against
            // `slot.id` (e.g. `clear_last_pasted_if`).
            *slot = None;
            return None;
        }
        Some(id)
    }

    /// Clear the last-pasted slot if it currently holds `id`. Called after
    /// any path that removes the entry (single delete, bulk delete) so the
    /// next "repaste last" falls through to the recency fallback rather
    /// than failing with `NotFound`.
    pub fn clear_last_pasted_if(&self, id: EntryId) {
        let mut slot = self
            .last_pasted_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((stored_id, _)) = *slot
            && stored_id == id
        {
            *slot = None;
        }
    }

    /// Clear the last-pasted slot unconditionally. Used by `clear_history`
    /// and other bulk-purge paths where any tracked id is presumed gone.
    pub fn clear_last_pasted(&self) {
        let mut slot = self
            .last_pasted_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = None;
    }

    /// Paste the tracked last-pasted entry, falling back to the recency
    /// head when none is tracked or the tracked id has been retention-swept.
    /// Returns `AppError::NotFound` when neither path has anything to paste
    /// (no last-pasted slot and an empty history).
    pub async fn repaste_last_or_recency(&self) -> Result<()> {
        if let Some(id) = self.last_pasted() {
            match self.runtime.paste_entry(id, None).await {
                Ok(()) => {
                    self.record_last_pasted(id);
                    return Ok(());
                }
                Err(AppError::NotFound) => self.clear_last_pasted_if(id),
                Err(err) => return Err(err),
            }
        }
        let entries = self.runtime.list_recent(1).await?;
        let Some(entry) = entries.into_iter().next() else {
            return Err(AppError::NotFound);
        };
        self.runtime.paste_entry(entry.id, None).await?;
        self.record_last_pasted(entry.id);
        Ok(())
    }
}

impl AppState {
    pub fn try_new() -> Result<Self> {
        let db_path = default_db_path();
        Self::try_new_at(&db_path)
    }

    /// Open the store at `db_path` and wrap any failure with that path and
    /// recovery guidance. The setup closure prints these errors directly to
    /// the user, so the message must be self-explanatory: which file failed,
    /// which permission was needed, and what command will move the broken
    /// DB aside without losing data.
    pub fn try_new_at(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            nagori_storage::ensure_private_directory(parent)
                .map_err(|err| annotate_startup_error(err, db_path, StartupStage::Directory))?;
        }
        // Take the single-instance lock *before* opening the store, so a
        // second launch never runs migrations or starts a capture loop
        // against a DB the first instance already owns. The lock lives in the
        // DB's parent directory, which the daemon also locks on the default
        // layout, so launching the app while a standalone daemon owns the same
        // store is refused too.
        let instance_lock = acquire_instance_lock(lock_dir_for(db_path))?;
        let store = SqliteStore::open(db_path)
            .map_err(|err| annotate_startup_error(err, db_path, StartupStage::OpenDb))?;
        let mut state = Self::build(store)?;
        state.instance_lock = Some(instance_lock);
        state.clear_on_quit_marker = Some(clear_on_quit_marker_path(db_path));
        // Fail-closed: complete any clear-on-quit purge the previous session
        // could not finish before the state is handed to Tauri. A failure here
        // surfaces the startup fallback window (and leaves the marker) rather
        // than booting into a session that still shows the cleared history.
        state.finish_pending_clear_on_quit()?;
        Ok(state)
    }

    fn build(store: SqliteStore) -> Result<Self> {
        let parts = build_native_runtime(store, NativeRuntimeOptions::default())?;
        let (settings_load_tx, settings_load_rx) =
            tokio::sync::watch::channel(SettingsLoadGate::Pending);
        Ok(Self {
            runtime: parts.runtime,
            window: parts.window,
            preview: parts.preview,
            capture_reader: parts.clipboard_reader,
            background_tasks: Mutex::new(None),
            previous_frontmost: Arc::new(Mutex::new(None)),
            last_pasted_id: Mutex::new(None),
            last_hotkey_failure: Mutex::new(HotkeyFailureCache::default()),
            // Set by `try_new_at` for real launches; `build`-only callers
            // (tests, in-memory stores) own no on-disk directory to lock.
            instance_lock: None,
            clear_on_quit_marker: None,
            settings_load_rx,
            settings_load_tx: Mutex::new(Some(settings_load_tx)),
        })
    }

    /// Record a hotkey registration failure so a later-mounted listener
    /// can re-hydrate it. Primary lands in the single slot; secondaries
    /// are keyed by action wire value so two simultaneously-failing
    /// secondaries don't overwrite each other.
    pub fn record_hotkey_failure(&self, record: HotkeyFailureRecord) {
        self.last_hotkey_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(record);
    }

    /// Clear the cached hotkey failure for the slot matching
    /// `(kind, action)`. Returns `true` if a record was actually cleared
    /// so the caller can emit a paired resolved event without a redundant
    /// poll. `(None, _)` clears the primary slot; `(Some("secondary"),
    /// Some(action))` clears that exact secondary entry. A primary
    /// success cannot wipe any secondary (and vice versa), and a
    /// secondary success only wipes its own action's entry — sibling
    /// actions sharing the kind keep their cached failures.
    pub fn clear_hotkey_failure_for_kind_action(
        &self,
        kind: Option<&str>,
        action: Option<&str>,
    ) -> bool {
        self.last_hotkey_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear_for_kind_action(kind, action)
    }

    /// Read the most-relevant cached hotkey failure for hydration on a
    /// late-mounted listener. The frontend store is single-slot, so
    /// when both kinds are failing simultaneously we prioritise
    /// primary — the palette toggle being broken is strictly more
    /// disruptive than a missing secondary action.
    pub fn current_hotkey_failure(&self) -> Option<HotkeyFailureRecord> {
        self.last_hotkey_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .most_relevant()
            .cloned()
    }

    /// Snapshot the full cache (both kinds) so callers reconciling
    /// against current settings can decide which slots are now stale
    /// without holding the mutex across the comparison.
    pub fn hotkey_failure_cache_snapshot(&self) -> HotkeyFailureCache {
        self.last_hotkey_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Environment variable that overrides the default DB path resolution.
///
/// Mirrors the recovery hint baked into [`annotate_startup_error`]: when
/// the platform default directory is unwritable, the user can point
/// nagori at a path they control without rebuilding. The same variable
/// is honoured by `crates/nagori-cli/src/main.rs::default_db_path` so
/// the CLI and desktop processes target the same store when both are
/// configured against it.
pub const NAGORI_DB_PATH_ENV: &str = "NAGORI_DB_PATH";

pub fn default_db_path() -> PathBuf {
    resolve_default_db_path(std::env::var_os(NAGORI_DB_PATH_ENV), dirs::data_local_dir())
}

/// Pure path-resolution helper so unit tests don't have to mutate the
/// process environment (which is `unsafe` and races with parallel tests).
fn resolve_default_db_path(
    override_env: Option<std::ffi::OsString>,
    data_local_dir: Option<PathBuf>,
) -> PathBuf {
    if let Some(value) = override_env
        && !value.is_empty()
    {
        return PathBuf::from(value);
    }
    data_local_dir
        .unwrap_or_else(|| PathBuf::from("."))
        .join("nagori")
        .join("nagori.sqlite")
}

/// Directory whose `nagori.lock` the single-instance lock lives in: the DB's
/// parent, falling back to the current directory for a bare relative DB
/// filename. Kept in lockstep with the daemon's choice
/// (`nagori_daemon::serve::acquire_daemon_lock`, keyed on the socket parent)
/// so that on the default layout — DB and socket share one directory — the
/// app and a standalone daemon contend for the same lock file.
fn lock_dir_for(db_path: &Path) -> &Path {
    match db_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Acquire the single-instance lock over `lock_dir`, mapping contention to a
/// self-explanatory startup error. The `setup` closure surfaces this message
/// in the fallback window (the only UI a duplicate launch reaches), so it must
/// tell the user where the running instance is rather than failing silently.
fn acquire_instance_lock(lock_dir: &Path) -> Result<nagori_storage::ProcessLock> {
    match nagori_storage::ProcessLock::try_acquire(lock_dir)? {
        Some(lock) => Ok(lock),
        None => Err(AppError::Platform(format!(
            "another nagori process is already using the clipboard history in {}. \
             Only one instance can own it at a time — look for the running nagori in \
             your menu bar / system tray, or use the global shortcut to open the \
             palette.",
            lock_dir.display()
        ))),
    }
}

#[derive(Debug, Clone, Copy)]
enum StartupStage {
    Directory,
    OpenDb,
}

/// Wrap a storage failure with the file path that caused it plus a one-line
/// recovery hint. Tauri's `setup` closure has no UI yet, so the only way to
/// guide the user through DB corruption / permission errors is to put the
/// command they need to run into the error string itself.
fn annotate_startup_error(err: AppError, db_path: &Path, stage: StartupStage) -> AppError {
    let path = db_path.display();
    let hint = match stage {
        StartupStage::Directory => format!(
            "could not prepare clipboard data directory for {path}: {err}. \
             Check that the parent directory is writable, or set NAGORI_DB_PATH \
             to a path you control"
        ),
        StartupStage::OpenDb => format!(
            "could not open clipboard database at {path}: {err}. \
             If the file is corrupted, move it aside (e.g. `mv \"{path}\" \
             \"{path}.broken\"`) and relaunch nagori — a fresh DB will be created"
        ),
    };
    // The hint repeats the original error's Display form for the dialog, but
    // keep the typed cause (`rusqlite::Error`, `io::Error`, …) in the source
    // chain instead of flattening it into the string.
    AppError::storage_with(hint, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary_record(hotkey: &str) -> HotkeyFailureRecord {
        HotkeyFailureRecord {
            hotkey: hotkey.to_owned(),
            error: "boom".to_owned(),
            kind: None,
            action: None,
        }
    }

    fn secondary_record(hotkey: &str, action: &str) -> HotkeyFailureRecord {
        HotkeyFailureRecord {
            hotkey: hotkey.to_owned(),
            error: "boom".to_owned(),
            kind: Some("secondary".to_owned()),
            action: Some(action.to_owned()),
        }
    }

    #[test]
    fn hotkey_cache_keeps_primary_and_secondary_independently() {
        // A secondary failure cached first must survive a primary
        // failure landing next — a single-slot cache would silently
        // overwrite it, hiding an unresolved binding from any later
        // hydration.
        let mut cache = HotkeyFailureCache::default();
        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        cache.record(primary_record("Cmd+Shift+V"));
        assert_eq!(
            cache.primary.as_ref().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+V")
        );
        assert_eq!(
            cache
                .secondary
                .get("repaste-last")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
    }

    #[test]
    fn hotkey_cache_keeps_two_secondaries_independently() {
        // Two secondary actions failing simultaneously is the bug a
        // single-slot secondary cache silently hides: the later record
        // overwrites the first and the user only ever sees one banner,
        // with the other failure permanently absent from hydration.
        let mut cache = HotkeyFailureCache::default();
        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        cache.record(secondary_record("Cmd+Shift+K", "clear-history"));
        assert_eq!(
            cache
                .secondary
                .get("repaste-last")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
        assert_eq!(
            cache
                .secondary
                .get("clear-history")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+K")
        );
    }

    #[test]
    fn hotkey_cache_drops_secondary_record_missing_action() {
        // The per-action map needs an addressable key. A secondary
        // record with no action identifier would be permanently
        // unclearable, so we drop it on insert rather than caching a
        // stuck entry. This shape is not produced by current emit
        // sites but guards against future regressions.
        let mut cache = HotkeyFailureCache::default();
        cache.record(HotkeyFailureRecord {
            hotkey: "Cmd+Shift+R".to_owned(),
            error: "boom".to_owned(),
            kind: Some("secondary".to_owned()),
            action: None,
        });
        assert!(cache.secondary.is_empty());
    }

    #[test]
    fn hotkey_cache_clear_for_kind_action_only_touches_matching_slot() {
        // Clearing primary must leave secondary alone (and vice versa)
        // so a primary success cannot silently wipe a still-failing
        // secondary from cache + hydration. Likewise, clearing one
        // secondary action must not affect a sibling action's entry.
        let mut cache = HotkeyFailureCache::default();
        cache.record(primary_record("Cmd+Shift+V"));
        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        cache.record(secondary_record("Cmd+Shift+K", "clear-history"));

        assert!(cache.clear_for_kind_action(None, None));
        assert!(cache.primary.is_none());
        assert_eq!(
            cache
                .secondary
                .get("repaste-last")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
        assert_eq!(
            cache
                .secondary
                .get("clear-history")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+K")
        );

        // Second primary clear is a no-op (already empty) — caller
        // uses this to decide whether to emit a resolved event.
        assert!(!cache.clear_for_kind_action(None, None));

        assert!(cache.clear_for_kind_action(Some("secondary"), Some("repaste-last")));
        assert!(!cache.secondary.contains_key("repaste-last"));
        assert_eq!(
            cache
                .secondary
                .get("clear-history")
                .map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+K")
        );
        assert!(!cache.clear_for_kind_action(Some("secondary"), Some("repaste-last")));

        // A blanket secondary clear (no action) must be a no-op — the
        // bug we are guarding against.
        assert!(!cache.clear_for_kind_action(Some("secondary"), None));
        assert!(cache.secondary.contains_key("clear-history"));
    }

    #[test]
    fn hotkey_cache_most_relevant_prefers_primary() {
        // The hydration path returns a single failure to a single-slot
        // frontend store. Primary takes priority because the palette
        // toggle being broken is strictly more disruptive than a
        // missing secondary action.
        let mut cache = HotkeyFailureCache::default();
        assert!(cache.most_relevant().is_none());

        cache.record(secondary_record("Cmd+Shift+R", "repaste-last"));
        assert_eq!(
            cache.most_relevant().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );

        cache.record(primary_record("Cmd+Shift+V"));
        assert_eq!(
            cache.most_relevant().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+V")
        );

        // After primary resolves, hydration falls through to a still
        // unresolved secondary — the failure isn't lost to the next
        // window mount.
        assert!(cache.clear_for_kind_action(None, None));
        assert_eq!(
            cache.most_relevant().map(|r| r.hotkey.as_str()),
            Some("Cmd+Shift+R")
        );
    }

    /// `NAGORI_DB_PATH` is advertised in the startup-failure hint
    /// (`annotate_startup_error`). The resolver must actually honour it,
    /// otherwise the recovery instructions point at a no-op.
    #[test]
    fn resolve_default_db_path_honours_env_override() {
        let override_path = PathBuf::from("/custom/path/to/nagori.sqlite");
        let resolved = resolve_default_db_path(
            Some(override_path.as_os_str().to_owned()),
            Some(PathBuf::from("/should/be/ignored")),
        );
        assert_eq!(resolved, override_path);
    }

    /// Empty env value is treated as unset so a user who runs
    /// `NAGORI_DB_PATH= nagori` (intending to clear the override) doesn't
    /// end up writing the DB to a relative empty path under cwd.
    #[test]
    fn resolve_default_db_path_treats_empty_env_as_unset() {
        let resolved = resolve_default_db_path(
            Some(std::ffi::OsString::new()),
            Some(PathBuf::from("/data/local")),
        );
        assert_eq!(resolved, PathBuf::from("/data/local/nagori/nagori.sqlite"));
    }

    /// Falls back to the platform default when the env var is unset.
    #[test]
    fn resolve_default_db_path_uses_platform_default_when_env_unset() {
        let resolved = resolve_default_db_path(None, Some(PathBuf::from("/data/local")));
        assert_eq!(resolved, PathBuf::from("/data/local/nagori/nagori.sqlite"));
    }
}

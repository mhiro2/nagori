// Typed bindings for the Tauri command surface. The Rust side exposes the
// same names via `#[tauri::command]`.

import { invoke } from './tauri';
import type {
  AiActionId,
  AiActionResult,
  AppSettings,
  EntryDto,
  EntryPreviewDto,
  HotkeyFailure,
  PasteFormat,
  PermissionStatus,
  PlatformCapabilities,
  SearchRequest,
  SearchResponse,
  UpdateInfo,
} from './types';

export const searchClipboard = (request: SearchRequest): Promise<SearchResponse> =>
  invoke('search_clipboard', { request });

export const listRecent = (limit: number): Promise<EntryDto[]> =>
  invoke('list_recent_entries', { limit });

export const listPinned = (): Promise<EntryDto[]> => invoke('list_pinned_entries');

export const getEntry = (id: string): Promise<EntryDto | null> => invoke('get_entry', { id });

export const copyEntry = (id: string): Promise<void> => invoke('copy_entry', { id });

export const pasteEntry = (id: string, format?: PasteFormat): Promise<void> =>
  invoke('paste_entry', { id, format });

export const openPalette = (): Promise<void> => invoke('open_palette');

export const closePalette = (): Promise<void> => invoke('close_palette');

export const pasteEntryFromPalette = (entryId: string, format?: PasteFormat): Promise<void> =>
  invoke('paste_entry_from_palette', { entryId, format });

export const copyEntryFromPalette = (entryId: string): Promise<void> =>
  invoke('copy_entry_from_palette', { entryId });

export const getEntryPreview = (entryId: string, query?: string): Promise<EntryPreviewDto> =>
  invoke('get_entry_preview', { entryId, query: query?.trim() ? query : undefined });

// Expanded preview body (1 MiB cap). Backend rejects non-Public entries
// with a `forbidden` code; the UI gates the button accordingly so the
// promise rarely sees that error in practice.
export const getEntryPreviewFull = (entryId: string): Promise<EntryPreviewDto> =>
  invoke('get_entry_preview_full', { entryId });

export const addEntry = (text: string): Promise<EntryDto> => invoke('add_entry', { text });

export const deleteEntry = (id: string): Promise<void> => invoke('delete_entry', { id });

export const deleteEntries = (ids: string[]): Promise<number> => invoke('delete_entries', { ids });

export const copyEntriesCombined = (ids: string[]): Promise<void> =>
  invoke('copy_entries_combined', { ids });

export const clearHistory = (): Promise<number> => invoke('clear_history');

export const repasteLast = (): Promise<void> => invoke('repaste_last');

export const pinEntry = (id: string, pinned: boolean): Promise<void> =>
  invoke('pin_entry', { id, pinned });

export const runAiAction = (action: AiActionId, entryId: string): Promise<AiActionResult> =>
  invoke('run_ai_action', { action, entryId });

export const getSettings = (): Promise<AppSettings> => invoke('get_settings');

// Serialize `update_settings` IPC at the module level. `save_settings`
// inside the daemon writes through a multi-connection SQLite pool, so two
// in-flight calls can settle out of order — fine when one SettingsView
// owns the conversation, but two overlapping instances (a window
// unmounting while another opens) race the SQLite writes and the later
// dispatch can land first. Chaining each call off the tail of the
// previous IPC enforces submission-order writes globally, without
// requiring backend cooperation. Last-write-wins is preserved because
// the backend accepts full snapshots. The `.catch` after `next` detaches
// the queue tail from any rejection so a single failed save (e.g. an
// invalid hotkey) does not poison subsequent callers — they still chain
// off a resolved tail and dispatch normally.
let updateSettingsTail: Promise<unknown> = Promise.resolve();

export const updateSettings = (settings: AppSettings): Promise<void> => {
  const next = updateSettingsTail.then(() => invoke<void>('update_settings', { settings }));
  updateSettingsTail = next.catch(() => undefined);
  return next;
};

export const togglePalette = (): Promise<void> => invoke('toggle_palette');

export const hidePalette = (): Promise<void> => invoke('hide_palette');

// Show / hide the standalone Settings window. The window is declared
// with native decorations in `tauri.conf.json`, so the OS supplies the
// close button, title-bar drag, and Cmd+Tab / Alt+Tab membership — these
// commands only flip its visibility.
export const openSettingsWindow = (): Promise<void> => invoke('open_settings');

export const closeSettingsWindow = (): Promise<void> => invoke('close_settings');

export const getPermissions = (): Promise<PermissionStatus[]> => invoke('get_permissions');

export const getCapabilities = (): Promise<PlatformCapabilities> => invoke('get_capabilities');

export const setCaptureEnabled = (enabled: boolean): Promise<AppSettings> =>
  invoke('set_capture_enabled', { enabled });

export const saveAiResult = (text: string): Promise<EntryDto> => invoke('save_ai_result', { text });

// Phase A: replaces `open_accessibility_settings`. When `prompt` is true the
// macOS backend triggers `AXIsProcessTrustedWithOptions(prompt:YES)` which
// surfaces the system dialog the first time it's called; on subsequent calls
// the daemon falls back to opening the Privacy → Accessibility pane via
// `open(1)` so the user still has a one-click route. Other platforms return
// a synthetic Granted/Denied row.
export const requestAccessibility = (prompt: boolean): Promise<PermissionStatus> =>
  invoke('request_accessibility', { prompt });

// Public-only external URL open. Backend verifies sensitivity, entry-id
// vs. URL match, and scheme allowlist (https/http) before handing the
// URL to the platform's default opener; the renderer also gates the
// trigger to keep forged invokes out of the UI loop.
export const openUrlExternal = (entryId: string, url: string): Promise<void> =>
  invoke('open_url_external', { entryId, url });

export const previewEntry = (entryId: string): Promise<void> =>
  invoke('preview_entry', { entryId });

export const checkForUpdates = (): Promise<UpdateInfo | null> => invoke('check_for_updates');

// Backend cache mirror of the latest `nagori://hotkey_register_failed`
// emit. Returns `null` when the most recent (re-)registration succeeded
// — used by the always-on App-level subscriber on mount to recover from
// a startup race where the live emit fires before the listener attaches.
export const lastHotkeyFailure = (): Promise<HotkeyFailure | null> => invoke('last_hotkey_failure');

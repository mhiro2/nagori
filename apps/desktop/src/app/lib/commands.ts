// Typed bindings for the Tauri command surface. The Rust side exposes the
// same names via `#[tauri::command]`.

import { invoke } from './tauri';
import type {
  AiActionId,
  AiActionResult,
  AppSettings,
  EntryDto,
  EntryPreviewDto,
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

export const getEntryPreview = (entryId: string): Promise<EntryPreviewDto> =>
  invoke('get_entry_preview', { entryId });

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

export const updateSettings = (settings: AppSettings): Promise<void> =>
  invoke('update_settings', { settings });

export const togglePalette = (): Promise<void> => invoke('toggle_palette');

export const hidePalette = (): Promise<void> => invoke('hide_palette');

export const getPermissions = (): Promise<PermissionStatus[]> => invoke('get_permissions');

export const getCapabilities = (): Promise<PlatformCapabilities> => invoke('get_capabilities');

export const setCaptureEnabled = (enabled: boolean): Promise<AppSettings> =>
  invoke('set_capture_enabled', { enabled });

export const saveAiResult = (text: string): Promise<EntryDto> => invoke('save_ai_result', { text });

export const openAccessibilitySettings = (): Promise<void> => invoke('open_accessibility_settings');

export const checkForUpdates = (): Promise<UpdateInfo | null> => invoke('check_for_updates');

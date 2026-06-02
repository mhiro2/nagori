import {
  copyEntriesCombined as copyEntriesCombinedCmd,
  copyEntryFromPalette as copyEntryCmd,
  deleteEntries as deleteEntriesCmd,
  deleteEntry as deleteEntryCmd,
  pasteEntryFromPalette as pasteEntryCmd,
  pinEntry as pinEntryCmd,
  previewEntry as previewEntryCmd,
} from '../lib/commands';
import { describeError } from '../lib/errors';
import { isTauri } from '../lib/tauri';
import type { PasteFormat } from '../lib/types';
import { clearPasteDiagnostics } from './pasteDiagnostics.svelte';
import { clearMultiSelect, multiSelectState } from './searchMultiSelect.svelte';
import { cancelPendingQuery, runQuery, searchState } from './searchQuery.svelte';
import { currentSelection } from './searchSelection';
import { settingsState } from './settings.svelte';

const oppositeFormat = (): PasteFormat | undefined => {
  const current = settingsState.settings?.pasteFormatDefault;
  if (current === undefined) return undefined;
  return current === 'preserve' ? 'plain_text' : 'preserve';
};

export const confirmSelection = async (format?: PasteFormat): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  // The Tauri command hides the palette on its way out; drop any pending
  // debounced search so a keystroke typed within the 80 ms window before
  // Enter doesn't land a runQuery against the now-hidden webview.
  cancelPendingQuery();
  try {
    await pasteEntryCmd(target.id, format);
    // A clean paste makes any prior failure diagnostic stale — drop the
    // StatusBar chip so it doesn't linger across a now-working paste.
    clearPasteDiagnostics();
  } catch (err) {
    searchState.errorMessage = describeError(err);
  }
};

export const confirmSelectionWithAlternateFormat = async (): Promise<void> => {
  await confirmSelection(oppositeFormat());
};

export const copySelection = async (): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  // Same hide-on-return contract as `confirmSelection` — cancel before
  // the IPC so the debounce can't fire post-hide.
  cancelPendingQuery();
  try {
    await copyEntryCmd(target.id);
  } catch (err) {
    searchState.errorMessage = describeError(err);
  }
};

// Flip the pin flag on a single row and refresh so the new pin ordering
// lands. Shared by the keyboard/footer path (`togglePinSelection`, which
// acts on the highlighted row) and the per-row pin button
// (`togglePinAt`, which acts on whichever row was clicked regardless of
// the current keyboard selection). On failure we surface the error and
// skip the refresh so the row keeps its old state.
const applyPinToggle = async (target: { id: string; pinned: boolean }): Promise<void> => {
  try {
    await pinEntryCmd(target.id, !target.pinned);
  } catch (err) {
    searchState.errorMessage = describeError(err);
    return;
  }
  await runQuery(searchState.query);
  // `runQuery` snaps the cursor back to index 0, which would yank the selection
  // onto the newest entry after every pin — jarring and making repeated
  // toggling on one row impossible. Re-anchor to the entry we just toggled by
  // id so the cursor stays on it (or follows it to its new slot when a
  // pinned-first ordering floats it up). If the entry dropped out of the list
  // (e.g. unpinning under the Pinned filter), leave the index where the refresh
  // left it. ResultList leaves the scroll position alone on this same-query
  // refresh, so the viewport doesn't jump.
  const anchored = searchState.results.findIndex((r) => r.id === target.id);
  if (anchored >= 0) searchState.selectedIndex = anchored;
};

export const togglePinSelection = async (): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  await applyPinToggle(target);
};

export const togglePinAt = async (index: number): Promise<void> => {
  const target = searchState.results[index];
  if (!target || !isTauri()) return;
  await applyPinToggle(target);
};

export const deleteSelection = async (): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  try {
    await deleteEntryCmd(target.id);
  } catch (err) {
    searchState.errorMessage = describeError(err);
    return;
  }
  await runQuery(searchState.query);
};

export const previewSelection = async (): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  // Mirror the backend `Public`-only gate so non-public rows never
  // round-trip through the IPC and never materialise a temp file.
  // Suppress silently rather than flashing an error — the keybinding
  // is intentionally inert on those rows.
  if (target.sensitivity !== 'Public') return;
  try {
    await previewEntryCmd(target.id);
  } catch (err) {
    searchState.errorMessage = describeError(err);
  }
};

// Order the multi-selected ids by their position in the visible result
// list so the combined-copy text reads top-to-bottom. Selected ids that
// no longer appear in the list (concurrent reconcile) tail-append in
// insertion order — the daemon accepts any subset, so this is harmless.
const orderedMultiSelection = (): string[] => {
  const set = multiSelectState.selected;
  if (set.size === 0) return [];
  const ordered = searchState.results.map((r) => r.id).filter((id) => set.has(id));
  const seen = new Set(ordered);
  for (const id of set) {
    if (!seen.has(id)) ordered.push(id);
  }
  return ordered;
};

// `runQuery` resets `errorMessage` at request start, so re-apply the
// action's error after the refresh — but only when (a) the active query
// hasn't moved on, and (b) the refresh itself didn't surface its own
// error (which is more important to show).
const refreshPreservingError = async (
  capturedError: string | undefined,
  queryBeforeAction: string,
): Promise<void> => {
  await runQuery(searchState.query);
  if (capturedError === undefined) return;
  if (searchState.query !== queryBeforeAction) return;
  if (searchState.errorMessage !== undefined) return;
  searchState.errorMessage = capturedError;
};

const runBulkAction = async (perform: (ids: string[]) => Promise<unknown>): Promise<void> => {
  const ids = orderedMultiSelection();
  if (ids.length === 0 || !isTauri()) return;
  const queryBeforeAction = searchState.query;
  let actionError: string | undefined;
  try {
    await perform(ids);
  } catch (err) {
    actionError = describeError(err);
  }
  if (actionError === undefined) clearMultiSelect();
  await refreshPreservingError(actionError, queryBeforeAction);
};

export const copyMultiSelection = (): Promise<void> => runBulkAction(copyEntriesCombinedCmd);

export const deleteMultiSelection = (): Promise<void> => runBulkAction(deleteEntriesCmd);

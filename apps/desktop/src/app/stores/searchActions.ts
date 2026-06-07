import {
  copyEntriesCombined as copyEntriesCombinedCmd,
  copyEntryFromPalette as copyEntryCmd,
  deleteEntries as deleteEntriesCmd,
  deleteEntry as deleteEntryCmd,
  listPasteOptions,
  pasteEntryFromPalette as pasteEntryCmd,
  pasteEntryRepresentationFromPalette as pasteEntryRepresentationCmd,
  pinEntry as pinEntryCmd,
  previewEntry as previewEntryCmd,
} from '../lib/commands';
import { describeError } from '../lib/errors';
import { isTauri } from '../lib/tauri';
import type { PasteFormat, PasteOption } from '../lib/types';
import { clearPasteDiagnostics } from './pasteDiagnostics.svelte';
import {
  closePasteFormatPicker,
  openPasteFormatPicker,
  pasteFormatPickerGeneration,
  pasteFormatPickerState,
} from './pasteFormatPicker.svelte';
import { clearMultiSelect, multiSelectState } from './searchMultiSelect.svelte';
import { cancelPendingQuery, runQuery, searchState } from './searchQuery.svelte';
import { currentSelection } from './searchSelection';
import { settingsState } from './settings.svelte';

const oppositeFormat = (): PasteFormat | undefined => {
  const current = settingsState.settings?.pasteFormatDefault;
  if (current === undefined) return undefined;
  return current === 'preserve' ? 'plain_text' : 'preserve';
};

// Paste a specific entry id, sharing the hide-on-return + diagnostics contract.
// Callers capture the id up front so an async step (or the picker) can never let
// the live selection drift onto a different entry before the paste lands.
// `force` makes the backend synthesise the keystroke even with auto-paste off —
// set by the deliberate alternate-format chord, cleared for plain Enter.
const pasteEntryId = async (id: string, format?: PasteFormat, force = false): Promise<void> => {
  // The Tauri command hides the palette on its way out; drop any pending
  // debounced search so a keystroke typed within the 80 ms window before
  // the paste doesn't land a runQuery against the now-hidden webview.
  cancelPendingQuery();
  try {
    await pasteEntryCmd(id, format, force);
    // A clean paste makes any prior failure diagnostic stale — drop the
    // StatusBar chip so it doesn't linger across a now-working paste.
    clearPasteDiagnostics();
  } catch (err) {
    searchState.errorMessage = describeError(err);
  }
};

export const confirmSelection = async (format?: PasteFormat): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  // Plain Enter honours the user's auto-paste setting (no forced synthesis).
  await pasteEntryId(target.id, format);
};

export const confirmSelectionWithAlternateFormat = async (): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  // Open a representation picker only when the entry genuinely offers a choice
  // (≥2 distinct pasteable formats, e.g. a copied file that also carries an
  // image and a text label). Otherwise keep the lightweight alternate-format
  // paste so the common "paste as plain text" stays a single keystroke.
  // `listPasteOptions` is the authority on what is pasteable; a failure (the
  // entry vanished mid-keystroke) just falls back to the plain alternate paste.
  // Capture the dismiss generation before the query so a palette hide
  // (blur / Escape) landing while it is in flight bails this opener instead of
  // popping the picker open on a hidden window against a now-stale entry.
  const generation = pasteFormatPickerGeneration();
  let options: PasteOption[] = [];
  try {
    options = await listPasteOptions(target.id);
  } catch {
    options = [];
  }
  if (pasteFormatPickerGeneration() !== generation) return;
  if (options.length >= 2) {
    openPasteFormatPicker(target.id, options);
  } else {
    // No real choice — paste the *captured* entry in the alternate format.
    // Using `target.id` (not a fresh `currentSelection()`) keeps a selection
    // change during the options query from redirecting the paste. This chord is
    // a deliberate paste, so force synthesis regardless of the auto-paste
    // setting (consistent with selecting a format in the picker).
    await pasteEntryId(target.id, oppositeFormat(), true);
  }
};

// Apply a choice from the representation picker. `undefined` is the "keep
// original" row — re-offer every captured representation (explicit Preserve,
// regardless of the user's paste_format_default) — while an option pastes
// exactly its representation. Shares the hide-on-return + diagnostics contract;
// the target is the id captured when the picker opened, not the live selection.
export const confirmPasteFormat = async (option: PasteOption | undefined): Promise<void> => {
  const targetId = pasteFormatPickerState.targetId;
  closePasteFormatPicker();
  if (targetId === undefined || !isTauri()) return;
  if (option === undefined) {
    // Picking from the picker is a deliberate paste, so force synthesis.
    await pasteEntryId(targetId, 'preserve', true);
    return;
  }
  cancelPendingQuery();
  try {
    await pasteEntryRepresentationCmd(targetId, option.mime);
    clearPasteDiagnostics();
  } catch (err) {
    searchState.errorMessage = describeError(err);
  }
};

// Dismiss the picker without pasting (Escape / click-outside).
export const cancelPasteFormat = (): void => {
  closePasteFormatPicker();
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

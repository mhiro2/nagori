import {
  copyEntriesCombined as copyEntriesCombinedCmd,
  copyEntryFromPalette as copyEntryCmd,
  deleteEntries as deleteEntriesCmd,
  deleteEntry as deleteEntryCmd,
  pasteEntryFromPalette as pasteEntryCmd,
  pinEntry as pinEntryCmd,
} from '../lib/commands';
import { describeError } from '../lib/errors';
import { isTauri } from '../lib/tauri';
import type { PasteFormat } from '../lib/types';
import { clearMultiSelect, multiSelectState } from './searchMultiSelect.svelte';
import { runQuery, searchState } from './searchQuery.svelte';
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
  try {
    await pasteEntryCmd(target.id, format);
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
  try {
    await copyEntryCmd(target.id);
  } catch (err) {
    searchState.errorMessage = describeError(err);
  }
};

export const togglePinSelection = async (): Promise<void> => {
  const target = currentSelection();
  if (!target || !isTauri()) return;
  try {
    await pinEntryCmd(target.id, !target.pinned);
  } catch (err) {
    searchState.errorMessage = describeError(err);
    return;
  }
  await runQuery(searchState.query);
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

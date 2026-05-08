// User-driven actions on the current selection: paste, copy, pin/unpin,
// delete. Each action funnels Tauri command errors back into
// `searchState.errorMessage` so the status bar surfaces them uniformly,
// and pin/delete re-run the active query so the list reflects the new
// state of the row.

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

/// Order the multi-selected ids by their position in the visible result
/// list so the combined-copy text reads top-to-bottom (matching what the
/// user sees) rather than insertion order. Falls through to the raw set
/// when a selected id is no longer in the visible list — that can only
/// happen mid-reconcile and is harmless because the daemon will accept
/// any subset.
const orderedMultiSelection = (): string[] => {
  const set = multiSelectState.selected;
  if (set.size === 0) return [];
  const ordered = searchState.results.map((r) => r.id).filter((id) => set.has(id));
  for (const id of set) {
    if (!ordered.includes(id)) ordered.push(id);
  }
  return ordered;
};

// Run the active query and re-apply any error the bulk action surfaced.
// `runQuery` resets `errorMessage` at the start of its request, so without
// this dance the action's failure message would flash and disappear.
//
// `queryBeforeAction` must be captured by the caller BEFORE awaiting the
// bulk IPC, not after — the user can type a newer query during that await
// too, and we don't want to resurrect a stale error that arrived after
// they moved on. We restore the action error only when (a) the active
// query is still the one the user had before kicking off the action and
// (b) the refresh itself didn't surface its own error, since "I couldn't
// even reload the list" is more important to show than the original
// action failure.
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

export const copyMultiSelection = async (): Promise<void> => {
  const ids = orderedMultiSelection();
  if (ids.length === 0 || !isTauri()) return;
  const queryBeforeAction = searchState.query;
  let actionError: string | undefined;
  try {
    await copyEntriesCombinedCmd(ids);
  } catch (err) {
    actionError = describeError(err);
  }
  if (actionError === undefined) clearMultiSelect();
  // Even on failure: the daemon may have inserted a partial combined
  // entry, and either way `reconcileMultiSelect` (driven by runQuery)
  // is the only place that drops stale ids from the visible list.
  await refreshPreservingError(actionError, queryBeforeAction);
};

export const deleteMultiSelection = async (): Promise<void> => {
  const ids = orderedMultiSelection();
  if (ids.length === 0 || !isTauri()) return;
  const queryBeforeAction = searchState.query;
  let actionError: string | undefined;
  try {
    await deleteEntriesCmd(ids);
  } catch (err) {
    actionError = describeError(err);
  }
  if (actionError === undefined) clearMultiSelect();
  // Refresh on both branches: if the bulk delete aborted partway, the
  // earlier rows are already gone from the DB and the visible list
  // would otherwise still show them. Letting `runQuery` re-fetch and
  // `reconcileMultiSelect` prune the set keeps the UI honest about
  // what's actually left.
  await refreshPreservingError(actionError, queryBeforeAction);
};

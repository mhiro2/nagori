// User-driven actions on the current selection: paste, copy, pin/unpin,
// delete. Each action funnels Tauri command errors back into
// `searchState.errorMessage` so the status bar surfaces them uniformly,
// and pin/delete re-run the active query so the list reflects the new
// state of the row.

import {
  copyEntryFromPalette as copyEntryCmd,
  deleteEntry as deleteEntryCmd,
  pasteEntryFromPalette as pasteEntryCmd,
  pinEntry as pinEntryCmd,
} from '../lib/commands';
import { describeError } from '../lib/errors';
import { isTauri } from '../lib/tauri';
import type { PasteFormat } from '../lib/types';
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

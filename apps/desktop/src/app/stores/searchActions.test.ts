import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  copyEntriesCombined: vi.fn(),
  copyEntryFromPalette: vi.fn(),
  deleteEntries: vi.fn(),
  deleteEntry: vi.fn(),
  listPasteOptions: vi.fn(async () => []),
  pasteEntryFromPalette: vi.fn(),
  pasteEntryRepresentationFromPalette: vi.fn(),
  pinEntry: vi.fn(),
  previewEntry: vi.fn(),
  listRecent: vi.fn(async () => []),
  searchClipboard: vi.fn(),
}));

import {
  copyEntriesCombined,
  copyEntryFromPalette,
  deleteEntries,
  deleteEntry,
  listPasteOptions,
  pasteEntryFromPalette,
  pasteEntryRepresentationFromPalette,
  pinEntry,
  previewEntry,
  searchClipboard,
} from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { PasteOption, SearchResultDto } from '../lib/types';
import { closePasteFormatPicker, pasteFormatPickerState } from './pasteFormatPicker.svelte';
import {
  confirmPasteFormat,
  confirmSelection,
  confirmSelectionWithAlternateFormat,
  copyMultiSelection,
  copySelection,
  deleteMultiSelection,
  deleteSelection,
  previewSelection,
  togglePinAt,
  togglePinSelection,
} from './searchActions';
import { clearMultiSelect, multiSelectState, toggleMultiSelect } from './searchMultiSelect.svelte';
import { searchState } from './searchQuery.svelte';

const result = (overrides: Partial<SearchResultDto> = {}): SearchResultDto => ({
  id: 'r1',
  kind: 'text',
  preview: 'hello',
  score: 1,
  createdAt: '2026-05-05T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: ['Recent'],
  representationSummary: [],
  ...overrides,
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  // Bulk actions always re-run the active query after the IPC settles, so a
  // benign default keeps the post-call refresh from clobbering assertions.
  vi.mocked(searchClipboard).mockResolvedValue({
    results: [],
    totalCandidates: 0,
    searchElapsedMs: 0,
    summaryElapsedMs: 0,
    totalElapsedMs: 0,
  });
  searchState.query = '';
  searchState.results = [result()];
  searchState.selectedIndex = 0;
  searchState.loading = false;
  searchState.errorMessage = undefined;
  searchState.lastElapsedMs = undefined;
  clearMultiSelect();
  pasteFormatPickerState.open = false;
  pasteFormatPickerState.targetId = undefined;
  pasteFormatPickerState.options = [];
});

describe('confirmSelection', () => {
  it('pastes the current selection through the palette command', async () => {
    vi.mocked(pasteEntryFromPalette).mockResolvedValue();
    await confirmSelection();
    expect(pasteEntryFromPalette).toHaveBeenCalledWith('r1', undefined);
  });

  it('records an errorMessage when the paste command rejects', async () => {
    vi.mocked(pasteEntryFromPalette).mockRejectedValue(new Error('clipboard busy'));
    await confirmSelection();
    expect(searchState.errorMessage).toBe('clipboard busy');
  });

  it('skips the IPC when there is no selection', async () => {
    searchState.results = [];
    await confirmSelection();
    expect(pasteEntryFromPalette).not.toHaveBeenCalled();
  });
});

const pasteOption = (mime: string, category: PasteOption['category']): PasteOption => ({
  mime,
  category,
});

describe('confirmSelectionWithAlternateFormat', () => {
  it('opens the representation picker when the entry offers more than one format', async () => {
    vi.mocked(listPasteOptions).mockResolvedValue([
      pasteOption('text/html', 'html'),
      pasteOption('text/plain', 'plainText'),
    ]);
    await confirmSelectionWithAlternateFormat();
    expect(pasteFormatPickerState.open).toBe(true);
    expect(pasteFormatPickerState.targetId).toBe('r1');
    expect(pasteFormatPickerState.options).toHaveLength(2);
    // The picker defers the actual paste, so nothing is pasted yet.
    expect(pasteEntryFromPalette).not.toHaveBeenCalled();
  });

  it('falls back to the plain alternate paste when there is no real choice', async () => {
    vi.mocked(listPasteOptions).mockResolvedValue([pasteOption('text/plain', 'plainText')]);
    vi.mocked(pasteEntryFromPalette).mockResolvedValue();
    await confirmSelectionWithAlternateFormat();
    expect(pasteFormatPickerState.open).toBe(false);
    expect(pasteEntryFromPalette).toHaveBeenCalledTimes(1);
  });

  it('falls back to the plain alternate paste when listing options fails', async () => {
    vi.mocked(listPasteOptions).mockRejectedValue(new Error('entry vanished'));
    vi.mocked(pasteEntryFromPalette).mockResolvedValue();
    await confirmSelectionWithAlternateFormat();
    expect(pasteFormatPickerState.open).toBe(false);
    expect(pasteEntryFromPalette).toHaveBeenCalledTimes(1);
  });

  it('does not open the picker (nor paste) if the palette hides while options load', async () => {
    // A blur / Escape landing during the query dismisses the picker, bumping
    // the generation; the resolved opener must bail rather than pop a picker
    // onto the now-hidden palette against a stale target.
    vi.mocked(listPasteOptions).mockImplementation(async () => {
      closePasteFormatPicker();
      return [pasteOption('text/html', 'html'), pasteOption('text/plain', 'plainText')];
    });
    await confirmSelectionWithAlternateFormat();
    expect(pasteFormatPickerState.open).toBe(false);
    expect(pasteEntryFromPalette).not.toHaveBeenCalled();
  });
});

describe('confirmPasteFormat', () => {
  beforeEach(() => {
    pasteFormatPickerState.open = true;
    pasteFormatPickerState.targetId = 'r1';
    pasteFormatPickerState.options = [{ mime: 'image/png', category: 'image' }];
  });

  it('pastes the explicit Preserve format and closes the picker for "keep original"', async () => {
    vi.mocked(pasteEntryFromPalette).mockResolvedValue();
    await confirmPasteFormat(undefined);
    // "Keep original" must re-offer every representation regardless of the
    // user's default paste format, so it forces 'preserve' rather than omitting.
    expect(pasteEntryFromPalette).toHaveBeenCalledWith('r1', 'preserve');
    expect(pasteEntryRepresentationFromPalette).not.toHaveBeenCalled();
    expect(pasteFormatPickerState.open).toBe(false);
  });

  it('pastes exactly the chosen representation and closes the picker', async () => {
    vi.mocked(pasteEntryRepresentationFromPalette).mockResolvedValue();
    await confirmPasteFormat({ mime: 'image/png', category: 'image' });
    expect(pasteEntryRepresentationFromPalette).toHaveBeenCalledWith('r1', 'image/png');
    expect(pasteFormatPickerState.open).toBe(false);
  });

  it('surfaces a paste failure as an errorMessage', async () => {
    vi.mocked(pasteEntryRepresentationFromPalette).mockRejectedValue(new Error('cannot publish'));
    await confirmPasteFormat({ mime: 'image/png', category: 'image' });
    expect(searchState.errorMessage).toBe('cannot publish');
  });
});

describe('copySelection', () => {
  it('routes through copy_entry_from_palette and surfaces failures', async () => {
    vi.mocked(copyEntryFromPalette).mockRejectedValue(new Error('copy failed'));
    await copySelection();
    expect(searchState.errorMessage).toBe('copy failed');
  });
});

describe('togglePinSelection', () => {
  it('flips the pinned flag on the backend and refreshes the query', async () => {
    vi.mocked(pinEntry).mockResolvedValue();
    await togglePinSelection();
    expect(pinEntry).toHaveBeenCalledWith('r1', true);
  });

  it('records the error and skips refresh when pinEntry rejects', async () => {
    vi.mocked(pinEntry).mockRejectedValue(new Error('pin denied'));
    await togglePinSelection();
    expect(searchState.errorMessage).toBe('pin denied');
  });

  it('keeps the selection on the toggled entry after the refresh re-sorts', async () => {
    // 'b' is selected; pinning floats it to the top of the refreshed list.
    // Without re-anchoring, runQuery would reset the cursor to index 0 and
    // strand it on whatever now sits there — here it must follow 'b'.
    searchState.results = [result({ id: 'a' }), result({ id: 'b' })];
    searchState.selectedIndex = 1;
    vi.mocked(pinEntry).mockResolvedValue();
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [result({ id: 'b', pinned: true }), result({ id: 'a' })],
      totalCandidates: 2,
      searchElapsedMs: 0,
      summaryElapsedMs: 0,
      totalElapsedMs: 0,
    });
    await togglePinSelection();
    expect(pinEntry).toHaveBeenCalledWith('b', true);
    expect(searchState.selectedIndex).toBe(0);
    expect(searchState.results[searchState.selectedIndex]?.id).toBe('b');
  });
});

describe('togglePinAt', () => {
  it('flips the pinned flag on the row at the given index, not the selection', async () => {
    // Selection sits on index 0; togglePinAt targets a different row so the
    // per-row pin button can act independently of the keyboard cursor.
    searchState.results = [result({ id: 'a' }), result({ id: 'b', pinned: true })];
    searchState.selectedIndex = 0;
    vi.mocked(pinEntry).mockResolvedValue();
    await togglePinAt(1);
    expect(pinEntry).toHaveBeenCalledWith('b', false);
  });

  it('does nothing when the index is out of range', async () => {
    searchState.results = [result({ id: 'a' })];
    await togglePinAt(5);
    expect(pinEntry).not.toHaveBeenCalled();
  });
});

describe('deleteSelection', () => {
  it('drops the entry on the backend and refreshes', async () => {
    vi.mocked(deleteEntry).mockResolvedValue();
    await deleteSelection();
    expect(deleteEntry).toHaveBeenCalledWith('r1');
  });

  it('surfaces a delete failure without refreshing', async () => {
    vi.mocked(deleteEntry).mockRejectedValue(new Error('locked'));
    await deleteSelection();
    expect(searchState.errorMessage).toBe('locked');
  });
});

describe('previewSelection', () => {
  it('invokes preview_entry for a Public selection', async () => {
    vi.mocked(previewEntry).mockResolvedValue();
    await previewSelection();
    expect(previewEntry).toHaveBeenCalledWith('r1');
  });

  it('suppresses the IPC for non-Public sensitivity', async () => {
    // The backend would reject anyway, but mirroring the gate in the UI
    // avoids a round-trip plus the temp-file materialisation cost.
    searchState.results = [result({ sensitivity: 'Private' })];
    await previewSelection();
    expect(previewEntry).not.toHaveBeenCalled();
  });

  it('records the error if the backend rejects', async () => {
    vi.mocked(previewEntry).mockRejectedValue(new Error('qlmanage missing'));
    await previewSelection();
    expect(searchState.errorMessage).toBe('qlmanage missing');
  });
});

describe('multi-select bulk actions', () => {
  it('orders ids by visible position before calling copy_entries_combined', async () => {
    searchState.results = [result({ id: 'a' }), result({ id: 'b' }), result({ id: 'c' })];
    // Toggle in reverse order to make sure ordering reflects the list,
    // not the user's selection order.
    toggleMultiSelect('c');
    toggleMultiSelect('a');
    vi.mocked(copyEntriesCombined).mockResolvedValue();
    await copyMultiSelection();
    expect(copyEntriesCombined).toHaveBeenCalledWith(['a', 'c']);
  });

  it('clears the multi-select set after a successful bulk copy', async () => {
    searchState.results = [result({ id: 'a' }), result({ id: 'b' })];
    toggleMultiSelect('a');
    toggleMultiSelect('b');
    vi.mocked(copyEntriesCombined).mockResolvedValue();
    await copyMultiSelection();
    expect(multiSelectState.selected.size).toBe(0);
  });

  it('keeps the selection intact when a bulk copy fails', async () => {
    const visible = result({ id: 'a' });
    searchState.results = [visible];
    toggleMultiSelect('a');
    vi.mocked(copyEntriesCombined).mockRejectedValue(new Error('clipboard busy'));
    // The post-call refresh re-runs the active query; return the same row so
    // reconcileMultiSelect doesn't prune the still-visible selection.
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [visible],
      totalCandidates: 1,
      searchElapsedMs: 0,
      summaryElapsedMs: 0,
      totalElapsedMs: 0,
    });
    await copyMultiSelection();
    expect(searchState.errorMessage).toBe('clipboard busy');
    expect(multiSelectState.selected.has('a')).toBe(true);
  });

  it('routes deletion through delete_entries and clears the slot', async () => {
    searchState.results = [result({ id: 'a' }), result({ id: 'b' })];
    toggleMultiSelect('a');
    toggleMultiSelect('b');
    vi.mocked(deleteEntries).mockResolvedValue(2);
    await deleteMultiSelection();
    expect(deleteEntries).toHaveBeenCalledWith(['a', 'b']);
    expect(multiSelectState.selected.size).toBe(0);
  });

  it('skips IPC and does nothing when the multi-select set is empty', async () => {
    await copyMultiSelection();
    await deleteMultiSelection();
    expect(copyEntriesCombined).not.toHaveBeenCalled();
    expect(deleteEntries).not.toHaveBeenCalled();
  });

  it('does not resurrect an action error after the refresh itself fails', async () => {
    searchState.results = [result({ id: 'a' })];
    toggleMultiSelect('a');
    vi.mocked(copyEntriesCombined).mockRejectedValue(new Error('clipboard busy'));
    // The refresh fails too — that's the more important error to surface.
    vi.mocked(searchClipboard).mockRejectedValue(new Error('disk gone'));
    await copyMultiSelection();
    expect(searchState.errorMessage).toBe('disk gone');
  });

  it('does not resurrect an action error if the user moved on to a new query', async () => {
    searchState.results = [result({ id: 'a' })];
    toggleMultiSelect('a');
    // Simulate a newer query landing during the refresh by mutating the
    // active query before the post-action refresh checks it. The refresh
    // itself succeeds quietly, so without the queryBeforeAction guard
    // we'd resurrect 'clipboard busy' on top of the user's new query.
    vi.mocked(copyEntriesCombined).mockRejectedValue(new Error('clipboard busy'));
    vi.mocked(searchClipboard).mockImplementation(async () => {
      searchState.query = 'something else';
      return {
        results: [],
        totalCandidates: 0,
        searchElapsedMs: 0,
        summaryElapsedMs: 0,
        totalElapsedMs: 0,
      };
    });
    await copyMultiSelection();
    expect(searchState.errorMessage).toBeUndefined();
  });

  it('does not resurrect an action error if the user typed during the bulk IPC', async () => {
    searchState.results = [result({ id: 'a' })];
    toggleMultiSelect('a');
    // The user types a new query while the bulk IPC is still in flight.
    // The snapshot of the original query has to be taken BEFORE the
    // await — otherwise the post-refresh comparison would see the new
    // query as the "before" baseline and falsely restore the stale error.
    vi.mocked(copyEntriesCombined).mockImplementation(async () => {
      searchState.query = 'something else';
      throw new Error('clipboard busy');
    });
    await copyMultiSelection();
    expect(searchState.errorMessage).toBeUndefined();
  });
});

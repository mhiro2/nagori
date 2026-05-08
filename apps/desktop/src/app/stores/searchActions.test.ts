import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  copyEntriesCombined: vi.fn(),
  copyEntryFromPalette: vi.fn(),
  deleteEntries: vi.fn(),
  deleteEntry: vi.fn(),
  pasteEntryFromPalette: vi.fn(),
  pinEntry: vi.fn(),
  listRecent: vi.fn(async () => []),
  searchClipboard: vi.fn(),
}));

import {
  copyEntriesCombined,
  copyEntryFromPalette,
  deleteEntries,
  deleteEntry,
  pasteEntryFromPalette,
  pinEntry,
  searchClipboard,
} from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { SearchResultDto } from '../lib/types';
import {
  confirmSelection,
  copyMultiSelection,
  copySelection,
  deleteMultiSelection,
  deleteSelection,
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
    elapsedMs: 0,
  });
  searchState.query = '';
  searchState.results = [result()];
  searchState.selectedIndex = 0;
  searchState.loading = false;
  searchState.errorMessage = undefined;
  searchState.lastElapsedMs = undefined;
  clearMultiSelect();
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
      elapsedMs: 0,
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
      return { results: [], totalCandidates: 0, elapsedMs: 0 };
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

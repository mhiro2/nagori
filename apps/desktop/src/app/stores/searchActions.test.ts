import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  copyEntryFromPalette: vi.fn(),
  deleteEntry: vi.fn(),
  pasteEntryFromPalette: vi.fn(),
  pinEntry: vi.fn(),
  listRecent: vi.fn(async () => []),
  searchClipboard: vi.fn(),
}));

import {
  copyEntryFromPalette,
  deleteEntry,
  pasteEntryFromPalette,
  pinEntry,
} from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { SearchResultDto } from '../lib/types';
import {
  confirmSelection,
  copySelection,
  deleteSelection,
  togglePinSelection,
} from './searchActions';
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
  searchState.query = '';
  searchState.results = [result()];
  searchState.selectedIndex = 0;
  searchState.loading = false;
  searchState.errorMessage = undefined;
  searchState.lastElapsedMs = undefined;
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

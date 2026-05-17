import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  searchClipboard: vi.fn(),
  copyEntryFromPalette: vi.fn(),
  pasteEntryFromPalette: vi.fn(),
  getEntryPreview: vi.fn(),
  pinEntry: vi.fn(),
  deleteEntry: vi.fn(),
}));

import {
  copyEntryFromPalette as copyEntryCmd,
  deleteEntry as deleteEntryCmd,
  getEntryPreview,
  pasteEntryFromPalette as pasteEntryCmd,
  pinEntry as pinEntryCmd,
  searchClipboard,
} from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { EntryPreviewDto, SearchResultDto } from '../lib/types';
import {
  confirmSelection,
  copySelection,
  deleteSelection,
  togglePinSelection,
} from './searchActions';
import { hydratePreview, previewState } from './searchPreview.svelte';
import { refreshRecent, runQuery, searchState } from './searchQuery.svelte';
import {
  currentSelection,
  selectByIndex,
  selectFirst,
  selectLast,
  selectNext,
  selectPrev,
} from './searchSelection';

const sampleEntry = (id: string, preview = `value-${id}`) => ({
  id,
  kind: 'text' as const,
  preview,
  score: 0,
  createdAt: '2026-05-05T00:00:00Z',
  updatedAt: '2026-05-05T00:00:00Z',
  useCount: 0,
  pinned: false,
  sensitivity: 'Public' as const,
  rankReasons: ['Recent'] as const,
});

const resetState = () => {
  searchState.query = '';
  searchState.results = [];
  searchState.selectedIndex = 0;
  searchState.loading = false;
  searchState.errorMessage = undefined;
  searchState.lastElapsedMs = undefined;
  previewState.entryId = undefined;
  previewState.preview = undefined;
  previewState.loading = false;
  previewState.errorMessage = undefined;
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  resetState();
});

describe('refreshRecent', () => {
  it('loads results from recent search and resets the selection', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [sampleEntry('a'), sampleEntry('b')].map(toResult),
      totalCandidates: 2,
      elapsedMs: 5,
    });
    searchState.selectedIndex = 5;

    await refreshRecent();

    expect(searchClipboard).toHaveBeenCalledWith({ query: '', mode: 'Recent', limit: 50 });
    expect(searchState.results).toHaveLength(2);
    expect(searchState.results.at(0)?.id).toBe('a');
    expect(searchState.selectedIndex).toBe(0);
    expect(searchState.loading).toBe(false);
  });

  it('captures errors into errorMessage without throwing', async () => {
    vi.mocked(searchClipboard).mockRejectedValue({
      code: 'storage_error',
      message: 'boom',
      recoverable: true,
    });

    await refreshRecent();

    expect(searchState.errorMessage).toBeTruthy();
    expect(searchState.loading).toBe(false);
  });

  it('falls back to a fixture when not running under Tauri', async () => {
    vi.mocked(isTauri).mockReturnValue(false);

    await refreshRecent();

    expect(searchClipboard).not.toHaveBeenCalled();
    expect(searchState.results).toHaveLength(1);
    expect(searchState.results.at(0)?.id).toBe('fixture-1');
  });
});

describe('runQuery', () => {
  it('delegates to refreshRecent for empty queries', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [sampleEntry('a')].map(toResult),
      totalCandidates: 1,
      elapsedMs: 5,
    });

    await runQuery('   ');

    expect(searchClipboard).toHaveBeenCalledWith({ query: '', mode: 'Recent', limit: 50 });
    expect(searchState.query).toBe('   ');
  });

  it('dispatches non-empty queries to searchClipboard with Auto mode', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [
        {
          id: 'x',
          kind: 'text',
          preview: 'match',
          score: 0.9,
          createdAt: '2026-05-05T00:00:00Z',
          pinned: false,
          sensitivity: 'Public',
          rankReasons: ['FullTextMatch'],
          representationSummary: [],
        },
      ],
      totalCandidates: 1,
      elapsedMs: 12,
    });

    await runQuery('needle');

    expect(searchClipboard).toHaveBeenCalledWith({
      query: 'needle',
      mode: 'Auto',
      limit: 50,
    });
    expect(searchState.results).toHaveLength(1);
    expect(searchState.lastElapsedMs).toBe(12);
  });
});

describe('selection helpers', () => {
  beforeEach(() => {
    searchState.results = [sampleEntry('a'), sampleEntry('b'), sampleEntry('c')].map(toResult);
    searchState.selectedIndex = 0;
  });

  it('selectNext wraps to the start past the end', () => {
    selectNext();
    selectNext();
    expect(searchState.selectedIndex).toBe(2);
    selectNext();
    expect(searchState.selectedIndex).toBe(0);
  });

  it('selectPrev wraps to the end from the start', () => {
    selectPrev();
    expect(searchState.selectedIndex).toBe(2);
  });

  it('selectFirst and selectLast jump to the bounds', () => {
    selectLast();
    expect(searchState.selectedIndex).toBe(2);
    selectFirst();
    expect(searchState.selectedIndex).toBe(0);
  });

  it('selectByIndex ignores out-of-range targets', () => {
    selectByIndex(99);
    expect(searchState.selectedIndex).toBe(0);
    selectByIndex(-1);
    expect(searchState.selectedIndex).toBe(0);
    selectByIndex(2);
    expect(searchState.selectedIndex).toBe(2);
  });

  it('currentSelection returns the entry at the active index', () => {
    selectByIndex(1);
    expect(currentSelection()?.id).toBe('b');
  });

  it('selectNext on an empty list is a noop', () => {
    searchState.results = [];
    searchState.selectedIndex = 0;
    selectNext();
    selectPrev();
    selectLast();
    expect(searchState.selectedIndex).toBe(0);
  });
});

describe('action helpers', () => {
  beforeEach(() => {
    searchState.results = [sampleEntry('a'), { ...sampleEntry('b'), pinned: true }].map(toResult);
    searchState.selectedIndex = 0;
  });

  it('confirmSelection forwards the selected id to pasteEntry', async () => {
    await confirmSelection();
    expect(pasteEntryCmd).toHaveBeenCalledWith('a', undefined);
  });

  it('copySelection forwards the selected id to copyEntry', async () => {
    selectByIndex(1);
    await copySelection();
    expect(copyEntryCmd).toHaveBeenCalledWith('b');
  });

  it('togglePinSelection inverts the pinned flag and refreshes the query', async () => {
    selectByIndex(1);
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [],
      totalCandidates: 0,
      elapsedMs: 0,
    });
    await togglePinSelection();
    expect(pinEntryCmd).toHaveBeenCalledWith('b', false);
  });

  it('deleteSelection removes the entry then refreshes the query', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [],
      totalCandidates: 0,
      elapsedMs: 0,
    });
    await deleteSelection();
    expect(deleteEntryCmd).toHaveBeenCalledWith('a');
  });

  it('action helpers are noops outside of Tauri', async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    await confirmSelection();
    await copySelection();
    await togglePinSelection();
    await deleteSelection();
    expect(pasteEntryCmd).not.toHaveBeenCalled();
    expect(copyEntryCmd).not.toHaveBeenCalled();
    expect(pinEntryCmd).not.toHaveBeenCalled();
    expect(deleteEntryCmd).not.toHaveBeenCalled();
  });
});

describe('hydratePreview', () => {
  it('loads the selected entry preview and suppresses stale responses', async () => {
    let resolveA: ((value: EntryPreviewDto) => void) | undefined;
    vi.mocked(getEntryPreview)
      .mockReturnValueOnce(
        new Promise((resolve) => {
          resolveA = resolve;
        }),
      )
      .mockResolvedValueOnce({
        id: 'b',
        kind: 'text',
        title: null,
        previewText: 'preview b',
        body: { type: 'text', text: 'preview b' },
        metadata: {
          byteCount: 9,
          charCount: 9,
          lineCount: 1,
          truncated: false,
          sensitive: false,
          fullContentAvailable: true,
        },
      });

    const first = hydratePreview('a');
    const second = hydratePreview('b');
    expect(resolveA).toBeDefined();
    resolveA?.({
      id: 'a',
      kind: 'text',
      title: null,
      previewText: 'preview a',
      body: { type: 'text', text: 'preview a' },
      metadata: {
        byteCount: 9,
        charCount: 9,
        lineCount: 1,
        truncated: false,
        sensitive: false,
        fullContentAvailable: true,
      },
    });
    await Promise.all([first, second]);

    expect(previewState.entryId).toBe('b');
    expect(previewState.preview?.previewText).toBe('preview b');
  });
});

const toResult = (entry: ReturnType<typeof sampleEntry>): SearchResultDto => ({
  id: entry.id,
  kind: entry.kind,
  preview: entry.preview,
  score: entry.score,
  createdAt: entry.createdAt,
  pinned: entry.pinned,
  sensitivity: entry.sensitivity,
  rankReasons: [...entry.rankReasons],
  representationSummary: [],
});

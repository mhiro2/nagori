import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

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
import {
  resetSearchRuntimeForTest,
  flushPendingQuery,
  refreshRecent,
  runQuery,
  scheduleQuery,
  searchState,
} from './searchQuery.svelte';
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
  searchState.appliedQuery = '';
  searchState.results = [];
  searchState.selectedIndex = 0;
  searchState.loading = false;
  searchState.errorMessage = undefined;
  searchState.lastElapsedMs = undefined;
  previewState.entryId = undefined;
  previewState.preview = undefined;
  previewState.loading = false;
  previewState.errorMessage = undefined;
  // Reset the latest-only queue tickets so `flushPendingQuery`'s generation
  // gate doesn't inherit a prior test's failed/superseded search.
  resetSearchRuntimeForTest();
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
      searchElapsedMs: 5,
      summaryElapsedMs: 0,
      totalElapsedMs: 5,
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
      searchElapsedMs: 5,
      summaryElapsedMs: 0,
      totalElapsedMs: 5,
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
      // Distinct components so the `lastElapsedMs` assertion proves the UI
      // reads `totalElapsedMs` (12), not the search-only breakdown (7).
      searchElapsedMs: 7,
      summaryElapsedMs: 5,
      totalElapsedMs: 12,
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

describe('flushPendingQuery', () => {
  it('runs a pending debounced query, applies results, and reports ready', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [sampleEntry('fresh')].map(toResult),
      totalCandidates: 1,
      searchElapsedMs: 1,
      summaryElapsedMs: 0,
      totalElapsedMs: 1,
    });
    // The list still belongs to the previous query while the debounce is armed.
    searchState.results = [sampleEntry('stale')].map(toResult);
    searchState.appliedQuery = 'old';
    scheduleQuery('needle');
    expect(searchState.query).toBe('needle');
    expect(searchState.appliedQuery).toBe('old');

    const ready = await flushPendingQuery();

    // The pending query ran immediately and its results are now applied, so an
    // action reading the selection after the flush sees the typed query's list.
    expect(ready).toBe(true);
    expect(searchClipboard).toHaveBeenCalledWith({ query: 'needle', mode: 'Auto', limit: 50 });
    expect(searchState.appliedQuery).toBe('needle');
    expect(searchState.results.at(0)?.id).toBe('fresh');
  });

  it('reports not-ready when the pending search fails and leaves stale results', async () => {
    vi.mocked(searchClipboard).mockRejectedValue({
      code: 'storage_error',
      message: 'boom',
      recoverable: true,
    });
    searchState.results = [sampleEntry('stale')].map(toResult);
    searchState.appliedQuery = 'old';
    scheduleQuery('needle');

    const ready = await flushPendingQuery();

    // A failed search never advances `appliedQuery`, so the flush must report
    // not-ready and the caller must abort rather than act on the stale row.
    expect(ready).toBe(false);
    expect(searchState.appliedQuery).toBe('old');
  });

  it('is a noop and reports ready once a search has settled', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [sampleEntry('settled')].map(toResult),
      totalCandidates: 1,
      searchElapsedMs: 1,
      summaryElapsedMs: 0,
      totalElapsedMs: 1,
    });
    // A successful search advances the applied ticket to the latest request.
    await runQuery('needle');
    vi.mocked(searchClipboard).mockClear();

    const ready = await flushPendingQuery();

    expect(ready).toBe(true);
    // Nothing pending or stale, so the flush does not re-issue the search.
    expect(searchClipboard).not.toHaveBeenCalled();
  });

  it('retries the search on a later flush after an earlier failure', async () => {
    // First flush: the search fails, leaving the displayed list stale.
    vi.mocked(searchClipboard).mockRejectedValueOnce({
      code: 'storage_error',
      message: 'boom',
      recoverable: true,
    });
    scheduleQuery('needle');
    expect(await flushPendingQuery()).toBe(false);

    // A failure must not permanently wedge actions: the next flush re-issues
    // the query (now nothing pending/running), and a success makes it ready.
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [sampleEntry('recovered')].map(toResult),
      totalCandidates: 1,
      searchElapsedMs: 1,
      summaryElapsedMs: 0,
      totalElapsedMs: 1,
    });

    expect(await flushPendingQuery()).toBe(true);
    expect(searchState.appliedQuery).toBe('needle');
    expect(searchState.results.at(0)?.id).toBe('recovered');
  });

  it('makes confirmSelection abort when the pending search cannot settle', async () => {
    vi.mocked(searchClipboard).mockRejectedValue({
      code: 'storage_error',
      message: 'boom',
      recoverable: true,
    });
    searchState.results = [sampleEntry('stale')].map(toResult);
    searchState.selectedIndex = 0;
    searchState.appliedQuery = 'old';
    scheduleQuery('needle');

    await confirmSelection();

    // The stale row must never be pasted when the typed query's results are
    // unavailable.
    expect(pasteEntryCmd).not.toHaveBeenCalled();
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
    // Plain Enter: no explicit format, and does not force synthesis.
    expect(pasteEntryCmd).toHaveBeenCalledWith('a', undefined, false);
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
      searchElapsedMs: 0,
      summaryElapsedMs: 0,
      totalElapsedMs: 0,
    });
    await togglePinSelection();
    expect(pinEntryCmd).toHaveBeenCalledWith('b', false);
  });

  it('deleteSelection removes the entry then refreshes the query', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [],
      totalCandidates: 0,
      searchElapsedMs: 0,
      summaryElapsedMs: 0,
      totalElapsedMs: 0,
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

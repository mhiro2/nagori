import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

import { searchClipboard } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { SearchResponse, SearchResultDto } from '../lib/types';
import { sampleSearchResult } from '../test-helpers/fixtures';
import { clearFilters, togglePinnedOnly } from './searchFilters.svelte';
import {
  canLoadMoreResults,
  cancelPendingQuery,
  loadMoreResults,
  MAX_RESULT_LIMIT,
  RESULT_PAGE_SIZE,
  refreshRecent,
  resetSearchRuntimeForTest,
  resultLimitReached,
  resultsPaged,
  runQuery,
  scheduleQuery,
  searchState,
} from './searchQuery.svelte';

const result = (id: string): SearchResultDto => sampleSearchResult({ id, preview: `value-${id}` });

const response = (overrides: Partial<SearchResponse> = {}): SearchResponse => ({
  results: [],
  totalCandidates: 0,
  // Distinct values so a test asserting `lastElapsedMs` proves the UI reads
  // `totalElapsedMs` (12), not the search-only breakdown (5).
  searchElapsedMs: 5,
  summaryElapsedMs: 7,
  totalElapsedMs: 12,
  ...overrides,
});

const page = (count: number): SearchResultDto[] =>
  Array.from({ length: count }, (_, i) => result(`r${i}`));

// Answer every search with exactly as many rows as it asked for, the way a
// history larger than the cap would.
const fillEveryLimit = (): void => {
  vi.mocked(searchClipboard).mockImplementation(async (request) =>
    response({ results: page(request.limit ?? RESULT_PAGE_SIZE) }),
  );
};

// A promise whose resolution the test drives, so we can hold a backend search
// "in flight" and observe how concurrent requests coalesce around it.
const deferred = <T>(): { promise: Promise<T>; resolve: (value: T) => void } => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
};

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(isTauri).mockReturnValue(true);
  searchState.query = '';
  searchState.appliedQuery = '';
  searchState.results = [];
  searchState.resultLimit = RESULT_PAGE_SIZE;
  searchState.selectedIndex = 0;
  searchState.loading = false;
  searchState.errorMessage = undefined;
  searchState.lastElapsedMs = undefined;
});

afterEach(() => {
  vi.useRealTimers();
  resetSearchRuntimeForTest();
  clearFilters();
});

describe('refreshRecent', () => {
  it('falls back to a local fixture outside the Tauri runtime', async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    await refreshRecent();
    expect(searchClipboard).not.toHaveBeenCalled();
    expect(searchState.results.length).toBeGreaterThan(0);
    expect(searchState.selectedIndex).toBe(0);
  });

  it('hydrates results from recent search inside Tauri', async () => {
    vi.mocked(searchClipboard).mockResolvedValue(response({ results: [result('a'), result('b')] }));
    await refreshRecent();
    expect(searchClipboard).toHaveBeenCalledWith({ query: '', mode: 'Recent', limit: 50 });
    expect(searchState.results).toHaveLength(2);
    expect(searchState.results[0]?.id).toBe('a');
    expect(searchState.loading).toBe(false);
  });

  it('records the error and stops loading when recent search rejects', async () => {
    vi.mocked(searchClipboard).mockRejectedValue(new Error('disk gone'));
    await refreshRecent();
    expect(searchState.errorMessage).toBe('disk gone');
    expect(searchState.loading).toBe(false);
  });
});

describe('runQuery', () => {
  it('delegates to refreshRecent when the query trims to empty', async () => {
    vi.mocked(searchClipboard).mockResolvedValue(response({ results: [result('only')] }));
    await runQuery('   ');
    expect(searchClipboard).toHaveBeenCalledWith({ query: '', mode: 'Recent', limit: 50 });
    expect(searchState.results[0]?.id).toBe('only');
  });

  it('calls searchClipboard with the typed query inside Tauri', async () => {
    vi.mocked(searchClipboard).mockResolvedValue(
      response({ results: [{ ...result('match'), score: 1, rankReasons: ['ExactMatch'] }] }),
    );
    await runQuery('match');
    expect(searchClipboard).toHaveBeenCalledWith({ query: 'match', mode: 'Auto', limit: 50 });
    expect(searchState.results[0]?.id).toBe('match');
    expect(searchState.lastElapsedMs).toBe(12);
  });

  it('uses a local-fixture filter outside the Tauri runtime', async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    await runQuery('zzzz-no-match');
    expect(searchClipboard).not.toHaveBeenCalled();
    // The fallback fixture's preview text should not contain that query.
    expect(searchState.results).toEqual([]);
  });

  it('surfaces a localized error when searchClipboard rejects', async () => {
    vi.mocked(searchClipboard).mockRejectedValue(new Error('search blew up'));
    await runQuery('boom');
    expect(searchState.errorMessage).toBe('search blew up');
  });
});

describe('selection across result sets', () => {
  it('keeps the selected entry when a same-query refresh inserts a newer row', async () => {
    // A background capture lands while row 'c' is selected: the refresh pushes
    // it down one slot, and the cursor must follow it rather than snap to the
    // new top entry that the next Enter would otherwise paste.
    searchState.appliedQuery = 'q';
    searchState.results = [result('a'), result('b'), result('c')];
    searchState.selectedIndex = 2;
    vi.mocked(searchClipboard).mockResolvedValue(
      response({ results: [result('new'), result('a'), result('b'), result('c')] }),
    );
    await runQuery('q');
    expect(searchState.selectedIndex).toBe(3);
    expect(searchState.results[searchState.selectedIndex]?.id).toBe('c');
  });

  it('keeps the selection on an empty-query refresh too', async () => {
    searchState.results = [result('a'), result('b')];
    searchState.selectedIndex = 1;
    vi.mocked(searchClipboard).mockResolvedValue(
      response({ results: [result('new'), result('a'), result('b')] }),
    );
    await refreshRecent();
    expect(searchState.results[searchState.selectedIndex]?.id).toBe('b');
  });

  it('stays at the same position when the selected entry left the list', async () => {
    searchState.appliedQuery = 'q';
    searchState.results = [result('a'), result('b'), result('c')];
    searchState.selectedIndex = 1;
    vi.mocked(searchClipboard).mockResolvedValue(response({ results: [result('a'), result('c')] }));
    await runQuery('q');
    expect(searchState.results[searchState.selectedIndex]?.id).toBe('c');
  });

  it('clamps to the last row when the selected tail entry left the list', async () => {
    searchState.appliedQuery = 'q';
    searchState.results = [result('a'), result('b')];
    searchState.selectedIndex = 1;
    vi.mocked(searchClipboard).mockResolvedValue(response({ results: [result('a')] }));
    await runQuery('q');
    expect(searchState.selectedIndex).toBe(0);
  });

  it('resets to the top when the query changes', async () => {
    searchState.appliedQuery = 'q';
    searchState.results = [result('a'), result('b')];
    searchState.selectedIndex = 1;
    vi.mocked(searchClipboard).mockResolvedValue(response({ results: [result('a'), result('b')] }));
    await runQuery('qq');
    expect(searchState.selectedIndex).toBe(0);
  });
});

describe('scheduleQuery + cancelPendingQuery', () => {
  it('mirrors the input into searchState.query immediately', () => {
    scheduleQuery('he');
    expect(searchState.query).toBe('he');
  });

  it('runs the query once after the debounce window elapses', async () => {
    vi.mocked(searchClipboard).mockResolvedValue(response());
    scheduleQuery('hel');
    scheduleQuery('hell');
    scheduleQuery('hello');
    expect(searchClipboard).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(120);
    expect(searchClipboard).toHaveBeenCalledTimes(1);
    expect(searchClipboard).toHaveBeenCalledWith({ query: 'hello', mode: 'Auto', limit: 50 });
  });

  it('cancelPendingQuery prevents a scheduled run from firing', async () => {
    vi.mocked(searchClipboard).mockResolvedValue(response());
    scheduleQuery('drop');
    cancelPendingQuery();
    await vi.advanceTimersByTimeAsync(200);
    expect(searchClipboard).not.toHaveBeenCalled();
  });

  it('runQuery preempts a scheduled debounced run', async () => {
    vi.mocked(searchClipboard).mockResolvedValue(response());
    scheduleQuery('debounced');
    await runQuery('explicit');
    await vi.advanceTimersByTimeAsync(200);
    expect(searchClipboard).toHaveBeenCalledTimes(1);
    expect(searchClipboard).toHaveBeenCalledWith({
      query: 'explicit',
      mode: 'Auto',
      limit: 50,
    });
  });
});

describe('latest-only search queue', () => {
  it('coalesces overlapping searches into the first plus the latest queued', async () => {
    const first = deferred<SearchResponse>();
    const latest = deferred<SearchResponse>();
    vi.mocked(searchClipboard)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(latest.promise);

    const p1 = runQuery('a'); // runs immediately
    const p2 = runQuery('b'); // queued behind 'a'
    const p3 = runQuery('c'); // supersedes the queued 'b'

    // Only 'a' has reached the backend while it is still in flight.
    expect(searchClipboard).toHaveBeenCalledTimes(1);
    expect(searchClipboard).toHaveBeenNthCalledWith(1, { query: 'a', mode: 'Auto', limit: 50 });

    // 'b' was superseded before running, so its promise settles without a call.
    await p2;
    expect(searchClipboard).toHaveBeenCalledTimes(1);

    first.resolve(response({ results: [result('a')] }));
    await p1;

    // Finishing 'a' drains the queued latest ('c'); 'b' never runs, and the UI
    // stays in the loading state across the coalesced run.
    expect(searchClipboard).toHaveBeenCalledTimes(2);
    expect(searchClipboard).toHaveBeenNthCalledWith(2, { query: 'c', mode: 'Auto', limit: 50 });
    expect(searchState.loading).toBe(true);
    // 'a' resolved while 'c' was queued, so its now-stale results must not be
    // applied — the list stays empty until 'c' completes.
    expect(searchState.results).toHaveLength(0);
    expect(searchState.appliedQuery).toBe('');

    latest.resolve(response({ results: [result('c')] }));
    await p3;
    expect(searchState.results).toHaveLength(1);
    expect(searchState.results[0]?.id).toBe('c');
    expect(searchState.appliedQuery).toBe('c');
    expect(searchState.loading).toBe(false);
  });

  it('applies only the latest query regardless of backend completion order', async () => {
    const older = deferred<SearchResponse>();
    const latest = deferred<SearchResponse>();
    vi.mocked(searchClipboard)
      .mockReturnValueOnce(older.promise)
      .mockReturnValueOnce(latest.promise);

    const p1 = runQuery('old'); // in flight
    const p2 = runQuery('new'); // queued behind 'old'

    // Resolve the queued search's backend response before the in-flight one to
    // model out-of-order completion. Serializing the invokes (plus the ticket
    // guard as defense-in-depth) guarantees the final state reflects 'new'.
    latest.resolve(response({ results: [result('fresh')] }));
    older.resolve(response({ results: [result('stale')] }));

    await Promise.all([p1, p2]);

    expect(searchState.results).toHaveLength(1);
    expect(searchState.results[0]?.id).toBe('fresh');
    expect(searchState.appliedQuery).toBe('new');
  });
});

describe('result paging', () => {
  it('offers more rows only when the page filled up', async () => {
    vi.mocked(searchClipboard).mockResolvedValue(response({ results: page(3) }));
    await runQuery('foo');
    expect(canLoadMoreResults()).toBe(false);
    expect(resultLimitReached()).toBe(false);

    fillEveryLimit();
    await runQuery('foo');
    expect(searchState.resultLimit).toBe(RESULT_PAGE_SIZE);
    expect(canLoadMoreResults()).toBe(true);
  });

  it('grows the limit a page at a time up to the backend cap', async () => {
    fillEveryLimit();
    await runQuery('foo');
    const limits: number[] = [];
    for (const _ of [1, 2, 3]) {
      expect(canLoadMoreResults()).toBe(true);
      // Each page depends on the previous one having applied.
      // oxlint-disable-next-line no-await-in-loop
      await loadMoreResults();
      limits.push(vi.mocked(searchClipboard).mock.lastCall?.[0].limit ?? 0);
    }
    expect(canLoadMoreResults()).toBe(false);
    expect(limits).toEqual([100, 150, MAX_RESULT_LIMIT]);
    expect(searchState.results).toHaveLength(MAX_RESULT_LIMIT);
    expect(resultLimitReached()).toBe(true);
    expect(vi.mocked(searchClipboard).mock.lastCall?.[0]).toMatchObject({
      query: 'foo',
      mode: 'Auto',
    });
  });

  it('reports a paged list once more than one page was requested', async () => {
    fillEveryLimit();
    await runQuery('foo');
    expect(resultsPaged()).toBe(false);
    // The second page comes back short: nothing more to load, but still paged.
    vi.mocked(searchClipboard).mockResolvedValueOnce(response({ results: page(70) }));
    await loadMoreResults();
    expect(canLoadMoreResults()).toBe(false);
    expect(resultsPaged()).toBe(true);
  });

  it('pages the recent listing when the query is empty', async () => {
    fillEveryLimit();
    await refreshRecent();
    await loadMoreResults();
    expect(searchClipboard).toHaveBeenLastCalledWith({ query: '', mode: 'Recent', limit: 100 });
  });

  it('keeps the expanded limit across same-query refreshes', async () => {
    fillEveryLimit();
    await runQuery('foo');
    await loadMoreResults();
    // A capture landing or a pin toggle re-runs the same query.
    await runQuery('foo');
    expect(vi.mocked(searchClipboard).mock.lastCall?.[0].limit).toBe(100);
  });

  it('starts a new query or filter set back at one page', async () => {
    fillEveryLimit();
    await runQuery('foo');
    await loadMoreResults();

    await runQuery('bar');
    expect(vi.mocked(searchClipboard).mock.lastCall?.[0].limit).toBe(RESULT_PAGE_SIZE);

    await loadMoreResults();
    togglePinnedOnly();
    await runQuery('bar');
    expect(vi.mocked(searchClipboard).mock.lastCall?.[0].limit).toBe(RESULT_PAGE_SIZE);
  });

  it('starts back at one page when returning to a previously expanded query', async () => {
    fillEveryLimit();
    await runQuery('foo');
    await loadMoreResults();
    await runQuery('bar');
    await runQuery('foo');
    expect(vi.mocked(searchClipboard).mock.lastCall?.[0].limit).toBe(RESULT_PAGE_SIZE);
  });

  it('ignores Show more while a newer search is still in flight', async () => {
    fillEveryLimit();
    await runQuery('foo');
    await loadMoreResults();
    expect(searchState.resultLimit).toBe(100);

    // A filter change starts a replacement search; until it lands the footer
    // still describes the old list, and paging must not apply its expanded
    // limit to the new filter set.
    const held = deferred<SearchResponse>();
    vi.mocked(searchClipboard).mockReturnValueOnce(held.promise);
    togglePinnedOnly();
    const replacement = runQuery('foo');
    const callsBefore = vi.mocked(searchClipboard).mock.calls.length;
    await loadMoreResults();
    expect(vi.mocked(searchClipboard).mock.calls.length).toBe(callsBefore);
    expect(vi.mocked(searchClipboard).mock.lastCall?.[0].limit).toBe(RESULT_PAGE_SIZE);
    held.resolve(response({ results: page(RESULT_PAGE_SIZE) }));
    await replacement;
    expect(searchState.resultLimit).toBe(RESULT_PAGE_SIZE);
  });

  it('keeps the cursor on the highlighted entry when it survives the re-rank', async () => {
    fillEveryLimit();
    await runQuery('foo');
    searchState.selectedIndex = 42;
    await loadMoreResults();
    expect(searchState.results[searchState.selectedIndex]?.id).toBe('r42');
  });
});

describe('result scope version', () => {
  it('advances on a new query or filter set but not on refreshes or paging', async () => {
    fillEveryLimit();
    const start = searchState.resultScopeVersion;
    await runQuery('foo');
    expect(searchState.resultScopeVersion).toBe(start + 1);

    // A same-scope refresh (a capture landing) and paging keep the version, so
    // the palette does not re-announce the count on every background change.
    await runQuery('foo');
    await loadMoreResults();
    expect(searchState.resultScopeVersion).toBe(start + 1);

    togglePinnedOnly();
    await runQuery('foo');
    expect(searchState.resultScopeVersion).toBe(start + 2);
    await runQuery('bar');
    expect(searchState.resultScopeVersion).toBe(start + 3);
  });
});

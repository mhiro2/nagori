import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  searchClipboard: vi.fn(),
}));

import { searchClipboard } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { SearchResponse, SearchResultDto } from '../lib/types';
import {
  cancelPendingQuery,
  refreshRecent,
  runQuery,
  scheduleQuery,
  searchState,
} from './searchQuery.svelte';

const result = (id: string): SearchResultDto => ({
  id,
  kind: 'text',
  preview: `value-${id}`,
  score: 0,
  createdAt: '2026-05-05T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: ['Recent'],
  representationSummary: [],
});

const response = (overrides: Partial<SearchResponse> = {}): SearchResponse => ({
  results: [],
  totalCandidates: 0,
  elapsedMs: 5,
  ...overrides,
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
  vi.mocked(isTauri).mockReturnValue(true);
  searchState.query = '';
  searchState.results = [];
  searchState.selectedIndex = 0;
  searchState.loading = false;
  searchState.errorMessage = undefined;
  searchState.lastElapsedMs = undefined;
});

afterEach(() => {
  vi.useRealTimers();
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
    expect(searchState.lastElapsedMs).toBe(5);
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

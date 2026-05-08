// Query state for the command palette: the typed query, the result list,
// loading/error indicators, and the debounce that throttles keystrokes
// before they hit the Tauri `search_clipboard` command. When the runtime
// is missing — e.g. plain `vite dev` — the store falls back to a local
// sample fixture so the UI remains demo-able.

import { searchClipboard } from '../lib/commands';
import { describeError } from '../lib/errors';
import { messages } from '../lib/i18n/index.svelte';
import { isTauri } from '../lib/tauri';
import type { SearchRequest, SearchResultDto } from '../lib/types';
import { currentFilters } from './searchFilters.svelte';
import { reconcileMultiSelect } from './searchMultiSelect.svelte';

const fallbackFixture = (): SearchResultDto[] => [
  {
    id: 'fixture-1',
    kind: 'text',
    preview: messages().palette.fallback,
    score: 0,
    createdAt: new Date().toISOString(),
    pinned: false,
    sensitivity: 'Public',
    rankReasons: ['Recent'],
  },
];

type SearchState = {
  query: string;
  results: SearchResultDto[];
  selectedIndex: number;
  loading: boolean;
  errorMessage: string | undefined;
  lastElapsedMs: number | undefined;
};

export const searchState = $state<SearchState>({
  query: '',
  results: [],
  selectedIndex: 0,
  loading: false,
  errorMessage: undefined,
  lastElapsedMs: undefined,
});

let inflight = 0;

// Debounce keystrokes before hitting `searchClipboard`. The backend
// serializes SQLite work behind a single `Mutex<Connection>`, so a burst of
// "n", "ne", "nee", "need", "needl", "needle" all racing each other only
// stalls the queue. 80ms is short enough to feel "as you type" and long
// enough to collapse a typical typing burst into one query.
const SEARCH_DEBOUNCE_MS = 80;
let pendingQueryTimer: ReturnType<typeof setTimeout> | undefined;
let pendingQueryRaw: string | undefined;

const setQuery = (raw: string): void => {
  // Skip the assignment when nothing changed so downstream `$derived` /
  // `$effect` chains don't re-run on every keystroke that didn't actually
  // mutate the value (IME composition, repeated arrow keys, etc.).
  if (searchState.query !== raw) searchState.query = raw;
};

const runSearch = async (request: SearchRequest): Promise<void> => {
  const ticket = ++inflight;
  searchState.loading = true;
  searchState.errorMessage = undefined;
  try {
    const filters = currentFilters();
    const response = await searchClipboard({
      ...request,
      ...(filters !== undefined ? { filters } : {}),
    });
    if (ticket !== inflight) return;
    searchState.results = response.results;
    searchState.selectedIndex = 0;
    searchState.lastElapsedMs = response.elapsedMs;
    reconcileMultiSelect(response.results.map((r) => r.id));
  } catch (err) {
    if (ticket !== inflight) return;
    searchState.errorMessage = describeError(err);
  } finally {
    if (ticket === inflight) searchState.loading = false;
  }
};

export const refreshRecent = async (): Promise<void> => {
  if (!isTauri()) {
    searchState.results = fallbackFixture();
    searchState.selectedIndex = 0;
    reconcileMultiSelect(searchState.results.map((r) => r.id));
    return;
  }
  await runSearch({ query: '', mode: 'Recent', limit: 50 });
};

/// Debounced entry point used by the search input. Cancels any pending
/// timer, mirrors the raw input into `searchState.query` immediately so the
/// box stays controlled, then schedules `runQuery` after the debounce
/// window. Callers that need synchronous behaviour (tests, programmatic
/// refresh after pin/delete) still call `runQuery` directly.
export const scheduleQuery = (raw: string): void => {
  setQuery(raw);
  pendingQueryRaw = raw;
  if (pendingQueryTimer !== undefined) {
    clearTimeout(pendingQueryTimer);
  }
  pendingQueryTimer = setTimeout(() => {
    pendingQueryTimer = undefined;
    const target = pendingQueryRaw;
    pendingQueryRaw = undefined;
    if (target === undefined) return;
    void runQuery(target);
  }, SEARCH_DEBOUNCE_MS);
};

/// Cancel any debounced query without running it. Use when the palette
/// closes so a stale keystroke doesn't fire after the user moves on.
export const cancelPendingQuery = (): void => {
  if (pendingQueryTimer !== undefined) {
    clearTimeout(pendingQueryTimer);
    pendingQueryTimer = undefined;
  }
  pendingQueryRaw = undefined;
};

export const runQuery = async (raw: string): Promise<void> => {
  // Any explicit run preempts a pending debounced one.
  if (pendingQueryTimer !== undefined) {
    clearTimeout(pendingQueryTimer);
    pendingQueryTimer = undefined;
    pendingQueryRaw = undefined;
  }
  setQuery(raw);
  if (raw.trim() === '') {
    await refreshRecent();
    return;
  }
  if (!isTauri()) {
    const lower = raw.toLowerCase();
    searchState.results = fallbackFixture().filter((r) => r.preview.toLowerCase().includes(lower));
    searchState.selectedIndex = 0;
    reconcileMultiSelect(searchState.results.map((r) => r.id));
    return;
  }
  await runSearch({ query: raw, mode: 'Auto', limit: 50 });
};

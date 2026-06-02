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
import { currentFilters, recordSourceApps } from './searchFilters.svelte';
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
    representationSummary: [],
  },
];

type SearchState = {
  query: string;
  // The query that the entries currently in `results` were produced for. Unlike
  // `query` (which `scheduleQuery` updates on the keystroke, before the debounced
  // search runs), this only changes when a result set is actually applied — so
  // consumers can tell a genuinely new search (scroll the list to the top) from
  // a same-query refresh such as a pin toggle, delete, or clipboard capture
  // (leave the scroll position alone) without racing the debounce.
  appliedQuery: string;
  results: SearchResultDto[];
  selectedIndex: number;
  loading: boolean;
  errorMessage: string | undefined;
  lastElapsedMs: number | undefined;
};

export const searchState = $state<SearchState>({
  query: '',
  appliedQuery: '',
  results: [],
  selectedIndex: 0,
  loading: false,
  errorMessage: undefined,
  lastElapsedMs: undefined,
});

// Latest-only search queue. The palette fires a backend search per debounced
// keystroke; on a large history a single search can outlast the debounce
// window, so without coalescing a fast typist stacks several concurrent SQLite
// scans whose results are all discarded but one. We keep at most one search in
// flight and remember only the most recent request that arrived while it was
// running — intermediate keystrokes' searches never reach the backend. The
// `inflight` ticket still tags every run so a late response (e.g. one that
// resolves after the palette moved on) is dropped as defense-in-depth.
let inflight = 0;
let searchRunning = false;
let queuedSearch: { request: SearchRequest; resolve: () => void } | undefined;

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

// Entry point for every search. Collapses a burst of overlapping requests to
// a single in-flight backend invoke plus at most one queued "latest". The
// returned promise resolves when this request's search settles, or — if it was
// superseded while queued — as soon as a newer request replaces it (its
// results would be stale, so there is nothing to await).
// A finished search may apply its results only if it is still the most recent
// request: its ticket has not been superseded by a later run and nothing newer
// is waiting in the queue.
const isFreshest = (ticket: number): boolean => ticket === inflight && queuedSearch === undefined;

const runSearch = (request: SearchRequest): Promise<void> => {
  if (searchRunning) {
    // Drop the previously queued request: only the newest pending one matters.
    // Resolve its awaiter so a caller blocked on it doesn't hang forever.
    queuedSearch?.resolve();
    return new Promise<void>((resolve) => {
      queuedSearch = { request, resolve };
    });
  }
  return executeSearch(request);
};

const executeSearch = async (request: SearchRequest): Promise<void> => {
  searchRunning = true;
  const ticket = ++inflight;
  searchState.loading = true;
  searchState.errorMessage = undefined;
  try {
    const filters = currentFilters();
    const response = await searchClipboard({
      ...request,
      ...(filters !== undefined ? { filters } : {}),
    });
    // Apply only when this is still the freshest request: the ticket must
    // match *and* no newer request must have been queued behind us while we
    // awaited. A queued request means the typed query already moved on, so
    // writing these now-stale results — even briefly — would let the user act
    // on the wrong list before the queued search overwrites it.
    if (isFreshest(ticket)) {
      searchState.results = response.results;
      searchState.appliedQuery = request.query;
      searchState.selectedIndex = 0;
      searchState.lastElapsedMs = response.totalElapsedMs;
      reconcileMultiSelect(response.results.map((r) => r.id));
      // Feed the source-app dropdown. When this search was itself app-filtered
      // the results only carry the active app, so the recorder retains the full
      // set last seen unfiltered instead of collapsing the menu to one app.
      recordSourceApps(
        response.results.map((r) => r.sourceAppName),
        filters?.sourceApp !== undefined,
      );
    }
  } catch (err) {
    if (isFreshest(ticket)) searchState.errorMessage = describeError(err);
  } finally {
    searchRunning = false;
    const next = queuedSearch;
    queuedSearch = undefined;
    if (next !== undefined) {
      // Drain the queued latest request, then settle its awaiter. Loading
      // stays true across the coalesced run so the UI doesn't flicker.
      void executeSearch(next.request).then(next.resolve);
    } else if (ticket === inflight) {
      searchState.loading = false;
    }
  }
};

export const refreshRecent = async (): Promise<void> => {
  if (!isTauri()) {
    searchState.results = fallbackFixture();
    searchState.appliedQuery = '';
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
    searchState.appliedQuery = raw;
    searchState.selectedIndex = 0;
    reconcileMultiSelect(searchState.results.map((r) => r.id));
    return;
  }
  await runSearch({ query: raw, mode: 'Auto', limit: 50 });
};

export const refreshCurrent = async (): Promise<void> => {
  await runQuery(searchState.query);
};

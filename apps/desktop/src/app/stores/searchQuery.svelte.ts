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
// Ticket of the most recently *applied* result set. Advanced only when a
// search actually publishes its results (the freshest branch below), never on
// failure or supersede. `appliedTicket === inflight` therefore means "the
// latest search initiated — for whatever query *and* filters — succeeded and is
// what's on screen", which is the precise gate `flushPendingQuery` needs: a
// failed re-search (even one that keeps the same query string after a filter
// change) leaves `appliedTicket` behind `inflight`, so an action can tell the
// displayed list is stale rather than trusting a query-string match.
let appliedTicket = 0;

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
      appliedTicket = ticket;
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

/// Force the displayed results to reflect the *typed* query before an action
/// reads the selection. For the ~80 ms debounce window — and while the search
/// for the latest keystroke is still in flight — `searchState.results` (and the
/// selection) still belong to the previous query, so a paste/copy/delete fired
/// in that window would act on a stale entry the user can no longer see in the
/// box. Flushing runs the pending query immediately and awaits the freshest
/// results so the action targets the list the user actually typed.
///
/// Returns whether it is now safe to act: `true` only when the latest search —
/// for the current query *and* filters — has actually applied its results. It
/// returns `false` when the flush could not settle: the search errored and left
/// the previous results in place, or it was superseded mid-flight (a newer
/// keystroke / filter change arrived, e.g. a fast double Enter or a concurrent
/// clipboard refresh), so its promise resolved before fresh results landed.
/// Callers must abort in that case rather than act on a stale row. A no-op
/// (`true`) when results are already settled, so the common "type, wait, act"
/// path adds no latency.
export const flushPendingQuery = async (): Promise<boolean> => {
  // Re-run the current query when the displayed list does not already reflect
  // it: a debounce is armed (`pendingQueryTimer`), a search is mid-flight
  // (`searchRunning`), or the most recent search never applied
  // (`appliedTicket !== inflight` — it failed or was superseded). The last case
  // is what stops a one-off search failure from permanently wedging actions:
  // each attempt re-issues the search rather than giving up forever.
  if (pendingQueryTimer !== undefined || searchRunning || appliedTicket !== inflight) {
    await runQuery(searchState.query);
  }
  // Ready iff nothing is still pending/queued *and* the most recent search
  // actually published its results (`appliedTicket === inflight`). A plain
  // `appliedQuery === query` would wrongly pass when a filter-change re-search
  // (same query string) failed and left the old-filter list in place; gating on
  // the applied ticket catches every "results don't reflect current intent"
  // case — failure, supersede, and filter change alike.
  return (
    pendingQueryTimer === undefined &&
    !searchRunning &&
    queuedSearch === undefined &&
    appliedTicket === inflight
  );
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

/// Test-only reset of the module-private search runtime (the latest-only queue
/// tickets and debounce timer) so each test starts from a clean slate. Without
/// it the `appliedTicket`/`inflight` generation counters leak across tests and
/// make `flushPendingQuery`'s readiness gate depend on prior tests' searches.
/// Not for production use — `searchState` itself is reset separately.
export const resetSearchRuntimeForTest = (): void => {
  if (pendingQueryTimer !== undefined) clearTimeout(pendingQueryTimer);
  pendingQueryTimer = undefined;
  pendingQueryRaw = undefined;
  inflight = 0;
  appliedTicket = 0;
  searchRunning = false;
  queuedSearch = undefined;
};

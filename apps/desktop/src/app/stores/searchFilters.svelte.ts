import type { ContentKind, SearchFilters } from '../lib/types';

// Date filtering is single-select (one window at a time); the other axes
// (kinds, source app, pinned) compose freely with it and each other.
export type DatePreset = 'none' | 'today' | 'yesterday' | 'last7days' | 'last30days';

// The content kinds surfaced as type chips. Each maps to exactly one
// `ContentKind`; `richText` / `unknown` are intentionally not filterable —
// they are edge kinds with no obvious chip and would only add noise.
export const FILTERABLE_KINDS: readonly ContentKind[] = [
  'text',
  'url',
  'code',
  'image',
  'fileList',
];

type FilterState = {
  datePreset: DatePreset;
  // Multi-select: empty means "any kind".
  kinds: ContentKind[];
  // Single-select source-app name; `undefined` means "any app".
  sourceApp: string | undefined;
  pinnedOnly: boolean;
};

export const filterState = $state<FilterState>({
  datePreset: 'none',
  kinds: [],
  sourceApp: undefined,
  pinnedOnly: false,
});

// Clicking the active date preset clears it, so the chip doubles as a clear
// gesture (matches the previous single-preset behaviour).
export const setDatePreset = (next: DatePreset): void => {
  filterState.datePreset = filterState.datePreset === next ? 'none' : next;
};

// Toggle a kind in/out of the multi-select set. Reassigns the array (rather
// than mutating in place) so `$state` proxies notify dependents reliably.
export const toggleKind = (kind: ContentKind): void => {
  filterState.kinds = filterState.kinds.includes(kind)
    ? filterState.kinds.filter((k) => k !== kind)
    : [...filterState.kinds, kind];
};

// Set (or clear, with `undefined`) the single-select source app. Assigns
// rather than toggles: the dropdown's "All apps" row is the explicit clear, so
// re-picking the active app should be a no-op (standard radio semantics) rather
// than a hidden toggle-off.
export const setSourceApp = (app: string | undefined): void => {
  filterState.sourceApp = app;
};

// Capacity for the source-app dropdown so a noisy result set can't make the
// menu unwieldy.
export const MAX_SOURCE_OPTIONS = 8;

// Candidate source apps for the dropdown. The live result set collapses to the
// single selected app once a source filter is applied, which would hide every
// other app and force a clear-then-reselect round trip just to switch apps. We
// instead remember the apps from the most recent search that was NOT
// source-app filtered — the complete set for the current query/date/kind — so
// the open menu keeps offering every app to switch to.
export const sourceAppOptions = $state<{ apps: string[] }>({ apps: [] });

// Record the apps seen in a completed search. A source-app-filtered search only
// returns the active app, so keep the previously-recorded set (just ensuring
// the active app is present) instead of shrinking the menu; an unfiltered
// search refreshes the full set. Dedupes in first-seen order and caps the list.
export const recordSourceApps = (
  resultApps: readonly (string | undefined)[],
  appFiltered: boolean,
): void => {
  if (appFiltered) {
    const active = filterState.sourceApp;
    if (active !== undefined && !sourceAppOptions.apps.includes(active)) {
      sourceAppOptions.apps = [active, ...sourceAppOptions.apps].slice(0, MAX_SOURCE_OPTIONS);
    }
    return;
  }
  const seen = new Set<string>();
  const apps: string[] = [];
  for (const name of resultApps) {
    if (name === undefined || seen.has(name)) continue;
    seen.add(name);
    apps.push(name);
    if (apps.length >= MAX_SOURCE_OPTIONS) break;
  }
  sourceAppOptions.apps = apps;
};

export const togglePinnedOnly = (): void => {
  filterState.pinnedOnly = !filterState.pinnedOnly;
};

export const clearFilters = (): void => {
  filterState.datePreset = 'none';
  filterState.kinds = [];
  filterState.sourceApp = undefined;
  filterState.pinnedOnly = false;
};

export const hasActiveFilters = (): boolean =>
  filterState.datePreset !== 'none' ||
  filterState.kinds.length > 0 ||
  filterState.sourceApp !== undefined ||
  filterState.pinnedOnly;

// `setDate` is calendar-aware (DST transitions add/subtract a real day,
// not a fixed 24h), so prefer it over fixed-ms math.
const midnightOffsetDays = (offset: number): Date => {
  const d = new Date();
  d.setDate(d.getDate() + offset);
  d.setHours(0, 0, 0, 0);
  return d;
};

// Translate a date preset into `created_after` / `created_before` bounds.
// "Yesterday" is the only bounded window; the rest are open-ended lower bounds.
const dateRange = (preset: DatePreset): { createdAfter?: string; createdBefore?: string } => {
  switch (preset) {
    case 'today':
      return { createdAfter: midnightOffsetDays(0).toISOString() };
    case 'yesterday':
      return {
        createdAfter: midnightOffsetDays(-1).toISOString(),
        createdBefore: midnightOffsetDays(0).toISOString(),
      };
    case 'last7days':
      return { createdAfter: midnightOffsetDays(-7).toISOString() };
    case 'last30days':
      return { createdAfter: midnightOffsetDays(-30).toISOString() };
    case 'none':
      break;
  }
  return {};
};

// Build the wire `SearchFilters` from the current chip state, or `undefined`
// when nothing is active so the search request omits the filter object
// entirely. Only set fields are assigned (never `undefined`) so the value
// stays valid under `exactOptionalPropertyTypes`, and the daemon's cache key —
// which compares the full filter struct — distinguishes each combination.
export const currentFilters = (): SearchFilters | undefined => {
  if (!hasActiveFilters()) return undefined;
  const { createdAfter, createdBefore } = dateRange(filterState.datePreset);
  const filters: SearchFilters = {};
  if (filterState.kinds.length > 0) filters.kinds = [...filterState.kinds];
  if (filterState.pinnedOnly) filters.pinnedOnly = true;
  if (filterState.sourceApp !== undefined) filters.sourceApp = filterState.sourceApp;
  if (createdAfter !== undefined) filters.createdAfter = createdAfter;
  if (createdBefore !== undefined) filters.createdBefore = createdBefore;
  return filters;
};

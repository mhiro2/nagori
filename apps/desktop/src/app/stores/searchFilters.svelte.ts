import { listSourceApps } from '../lib/commands';
import { isTauri } from '../lib/tauri';
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

// Candidate source apps for the dropdown: every app in the live history, most
// recently seen first, fetched from the backend rather than read off the result
// page. The page only holds the rows that matched (and, once a source filter
// applies, only the active app), so deriving the options from it would hide
// apps whose entries are older or outside the current query.
export const sourceAppOptions = $state<{ apps: string[] }>({ apps: [] });

// Refresh the app list. Called when the palette mounts and again whenever the
// dropdown opens, so apps first seen since mount show up without a restart. A
// failed fetch keeps the previous list: the dropdown also folds in the apps on
// screen, so it degrades to that instead of emptying.
export const refreshSourceApps = async (): Promise<void> => {
  if (!isTauri()) return;
  try {
    sourceAppOptions.apps = await listSourceApps();
  } catch {
    // Keep the last known list.
  }
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

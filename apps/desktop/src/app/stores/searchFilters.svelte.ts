// Filter preset state for the palette's quick-filter chips. The user
// picks one preset at a time (or none) — the backend's `SearchFilters`
// shape supports more complex combinations, but the palette-level UX
// stays single-select for now to keep the chip row legible.

import type { SearchFilters } from '../lib/types';

export type FilterPreset = 'none' | 'today' | 'last7days' | 'pinned';

type FilterState = {
  preset: FilterPreset;
};

export const filterState = $state<FilterState>({
  preset: 'none',
});

/// Toggle a preset on; clicking the active preset turns it off so the
/// chip row doubles as a clear gesture.
export const setFilterPreset = (next: FilterPreset): void => {
  filterState.preset = filterState.preset === next ? 'none' : next;
};

export const clearFilterPreset = (): void => {
  filterState.preset = 'none';
};

/// Build the `SearchFilters` payload the backend expects from the
/// active preset. Returns `undefined` when no preset is active so
/// callers can omit the field entirely from the IPC request.
export const currentFilters = (): SearchFilters | undefined => {
  if (filterState.preset === 'today') {
    const start = new Date();
    start.setHours(0, 0, 0, 0);
    return { createdAfter: start.toISOString() };
  }
  if (filterState.preset === 'last7days') {
    const start = new Date();
    start.setDate(start.getDate() - 7);
    start.setHours(0, 0, 0, 0);
    return { createdAfter: start.toISOString() };
  }
  if (filterState.preset === 'pinned') {
    return { pinnedOnly: true };
  }
  return undefined;
};

import type { SearchFilters } from '../lib/types';

export type FilterPreset = 'none' | 'today' | 'last7days' | 'pinned';

type FilterState = {
  preset: FilterPreset;
};

export const filterState = $state<FilterState>({
  preset: 'none',
});

// Clicking the active preset clears it, so the chip row doubles as a
// clear gesture.
export const setFilterPreset = (next: FilterPreset): void => {
  filterState.preset = filterState.preset === next ? 'none' : next;
};

export const clearFilterPreset = (): void => {
  filterState.preset = 'none';
};

// `setDate` is calendar-aware (DST transitions add/subtract a real day,
// not a fixed 24h), so prefer it over fixed-ms math.
const midnightOffsetDays = (offset: number): Date => {
  const d = new Date();
  d.setDate(d.getDate() + offset);
  d.setHours(0, 0, 0, 0);
  return d;
};

export const currentFilters = (): SearchFilters | undefined => {
  switch (filterState.preset) {
    case 'today':
      return { createdAfter: midnightOffsetDays(0).toISOString() };
    case 'last7days':
      return { createdAfter: midnightOffsetDays(-7).toISOString() };
    case 'pinned':
      return { pinnedOnly: true };
    case 'none':
      return undefined;
    default:
      return undefined;
  }
};

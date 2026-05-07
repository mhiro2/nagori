// Selection helpers operating on `searchState.selectedIndex`. Selection
// lives alongside `searchState.results` so it stays in lock-step with the
// list (a fresh result set always resets the cursor to 0); these helpers
// keep that mutation in one place.

import type { SearchResultDto } from '../lib/types';
import { searchState } from './searchQuery.svelte';

export const selectNext = (): void => {
  if (searchState.results.length === 0) return;
  searchState.selectedIndex = (searchState.selectedIndex + 1) % searchState.results.length;
};

export const selectPrev = (): void => {
  if (searchState.results.length === 0) return;
  const last = searchState.results.length - 1;
  searchState.selectedIndex =
    searchState.selectedIndex === 0 ? last : searchState.selectedIndex - 1;
};

export const selectFirst = (): void => {
  searchState.selectedIndex = 0;
};

export const selectLast = (): void => {
  if (searchState.results.length === 0) return;
  searchState.selectedIndex = searchState.results.length - 1;
};

export const selectByIndex = (index: number): void => {
  if (index < 0 || index >= searchState.results.length) return;
  searchState.selectedIndex = index;
};

export const currentSelection = (): SearchResultDto | undefined =>
  searchState.results[searchState.selectedIndex];

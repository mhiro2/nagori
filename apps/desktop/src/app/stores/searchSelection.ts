// Selection helpers operating on `searchState.selectedIndex`. Selection
// lives alongside `searchState.results` so it stays in lock-step with the
// list (a new query resets the cursor to 0, a same-query refresh keeps it on
// the selected entry); these helpers keep that mutation in one place.

import type { SearchResultDto } from '../lib/types';
import { releaseCursorToTop, searchState } from './searchQuery.svelte';

const moveCursor = (index: number): void => {
  releaseCursorToTop();
  searchState.selectedIndex = index;
};

export const selectNext = (): void => {
  if (searchState.results.length === 0) return;
  moveCursor((searchState.selectedIndex + 1) % searchState.results.length);
};

export const selectPrev = (): void => {
  if (searchState.results.length === 0) return;
  const last = searchState.results.length - 1;
  moveCursor(searchState.selectedIndex === 0 ? last : searchState.selectedIndex - 1);
};

export const selectFirst = (): void => {
  moveCursor(0);
};

export const selectLast = (): void => {
  if (searchState.results.length === 0) return;
  moveCursor(searchState.results.length - 1);
};

export const selectByIndex = (index: number): void => {
  if (index < 0 || index >= searchState.results.length) return;
  moveCursor(index);
};

export const currentSelection = (): SearchResultDto | undefined =>
  searchState.results[searchState.selectedIndex];

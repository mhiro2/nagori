import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  clearFilterPreset,
  currentFilters,
  filterState,
  setFilterPreset,
} from './searchFilters.svelte';

beforeEach(() => {
  clearFilterPreset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('setFilterPreset', () => {
  it('switches between presets and toggles off when re-selecting the active one', () => {
    setFilterPreset('today');
    expect(filterState.preset).toBe('today');
    setFilterPreset('pinned');
    expect(filterState.preset).toBe('pinned');
    setFilterPreset('pinned');
    expect(filterState.preset).toBe('none');
  });
});

describe('currentFilters', () => {
  it('returns undefined when no preset is active', () => {
    expect(currentFilters()).toBeUndefined();
  });

  it('emits a midnight-anchored createdAfter for "today"', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-05-08T15:30:00.000Z'));
    setFilterPreset('today');
    const filters = currentFilters();
    expect(filters?.createdAfter).toBeDefined();
    // The boundary is local midnight; assert it lands at the start of
    // the calendar day, not "now minus 24h".
    const after = new Date(filters?.createdAfter ?? '');
    expect(after.getHours()).toBe(0);
    expect(after.getMinutes()).toBe(0);
    expect(after.getSeconds()).toBe(0);
  });

  it('emits a 7-days-back createdAfter for "last7days"', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-05-08T15:30:00.000Z'));
    setFilterPreset('last7days');
    const filters = currentFilters();
    expect(filters?.createdAfter).toBeDefined();
    const after = new Date(filters?.createdAfter ?? '');
    const now = new Date();
    const diffDays = (now.getTime() - after.getTime()) / (24 * 60 * 60 * 1000);
    // We anchor at midnight 7 days back, so the diff is between 7 and 8 days
    // depending on local-time offset within the day.
    expect(diffDays).toBeGreaterThanOrEqual(7);
    expect(diffDays).toBeLessThan(8.1);
  });

  it('emits pinnedOnly for "pinned"', () => {
    setFilterPreset('pinned');
    expect(currentFilters()).toEqual({ pinnedOnly: true });
  });
});

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  clearFilters,
  currentFilters,
  filterState,
  hasActiveFilters,
  setDatePreset,
  setSourceApp,
  toggleKind,
  togglePinnedOnly,
} from './searchFilters.svelte';

beforeEach(() => {
  clearFilters();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('setDatePreset', () => {
  it('switches between date presets and toggles off when re-selecting the active one', () => {
    setDatePreset('today');
    expect(filterState.datePreset).toBe('today');
    setDatePreset('last7days');
    expect(filterState.datePreset).toBe('last7days');
    setDatePreset('last7days');
    expect(filterState.datePreset).toBe('none');
  });
});

describe('toggleKind', () => {
  it('adds and removes kinds from the multi-select set', () => {
    toggleKind('url');
    toggleKind('code');
    expect(filterState.kinds).toEqual(['url', 'code']);
    toggleKind('url');
    expect(filterState.kinds).toEqual(['code']);
  });
});

describe('setSourceApp', () => {
  it('selects a source app and clears it when re-selected', () => {
    setSourceApp('Chrome');
    expect(filterState.sourceApp).toBe('Chrome');
    setSourceApp('Slack');
    expect(filterState.sourceApp).toBe('Slack');
    setSourceApp('Slack');
    expect(filterState.sourceApp).toBeUndefined();
  });
});

describe('togglePinnedOnly / hasActiveFilters', () => {
  it('reports active state across every axis', () => {
    expect(hasActiveFilters()).toBe(false);
    togglePinnedOnly();
    expect(filterState.pinnedOnly).toBe(true);
    expect(hasActiveFilters()).toBe(true);
    togglePinnedOnly();
    expect(hasActiveFilters()).toBe(false);
    toggleKind('image');
    expect(hasActiveFilters()).toBe(true);
  });
});

describe('currentFilters', () => {
  it('returns undefined when nothing is active', () => {
    expect(currentFilters()).toBeUndefined();
  });

  it('emits a midnight-anchored createdAfter for "today"', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-05-08T15:30:00.000Z'));
    setDatePreset('today');
    const filters = currentFilters();
    expect(filters?.createdAfter).toBeDefined();
    // The boundary is local midnight; assert it lands at the start of
    // the calendar day, not "now minus 24h".
    const after = new Date(filters?.createdAfter ?? '');
    expect(after.getHours()).toBe(0);
    expect(after.getMinutes()).toBe(0);
    expect(after.getSeconds()).toBe(0);
  });

  it('emits a bounded window for "yesterday"', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-05-08T15:30:00.000Z'));
    setDatePreset('yesterday');
    const filters = currentFilters();
    expect(filters?.createdAfter).toBeDefined();
    expect(filters?.createdBefore).toBeDefined();
    const after = new Date(filters?.createdAfter ?? '');
    const before = new Date(filters?.createdBefore ?? '');
    // The window spans exactly one calendar day (both anchored at midnight).
    const spanDays = (before.getTime() - after.getTime()) / (24 * 60 * 60 * 1000);
    expect(spanDays).toBeCloseTo(1, 5);
    expect(after.getHours()).toBe(0);
    expect(before.getHours()).toBe(0);
  });

  it('emits a 30-days-back createdAfter for "last30days"', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-05-08T15:30:00.000Z'));
    setDatePreset('last30days');
    const filters = currentFilters();
    const after = new Date(filters?.createdAfter ?? '');
    const now = new Date();
    const diffDays = (now.getTime() - after.getTime()) / (24 * 60 * 60 * 1000);
    expect(diffDays).toBeGreaterThanOrEqual(30);
    expect(diffDays).toBeLessThan(31.1);
  });

  it('composes kinds, source app, and pinned into one filter object', () => {
    toggleKind('url');
    toggleKind('code');
    setSourceApp('Chrome');
    togglePinnedOnly();
    expect(currentFilters()).toEqual({
      kinds: ['url', 'code'],
      sourceApp: 'Chrome',
      pinnedOnly: true,
    });
  });

  it('omits inactive axes rather than emitting undefined fields', () => {
    togglePinnedOnly();
    expect(currentFilters()).toEqual({ pinnedOnly: true });
  });
});

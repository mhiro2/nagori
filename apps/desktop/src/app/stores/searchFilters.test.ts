import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());
vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

import { listSourceApps } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import {
  clearFilters,
  currentFilters,
  filterState,
  hasActiveFilters,
  refreshSourceApps,
  setDatePreset,
  setSourceApp,
  sourceAppOptions,
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
  it('assigns the source app and clears it only when passed undefined', () => {
    setSourceApp('Chrome');
    expect(filterState.sourceApp).toBe('Chrome');
    setSourceApp('Slack');
    expect(filterState.sourceApp).toBe('Slack');
    // Re-selecting the active app is a no-op (radio semantics), not a toggle-off.
    setSourceApp('Slack');
    expect(filterState.sourceApp).toBe('Slack');
    setSourceApp(undefined);
    expect(filterState.sourceApp).toBeUndefined();
  });
});

describe('refreshSourceApps', () => {
  beforeEach(() => {
    sourceAppOptions.apps = [];
    vi.mocked(isTauri).mockReturnValue(true);
  });

  it('loads the whole-history app list from the backend', async () => {
    vi.mocked(listSourceApps).mockResolvedValueOnce(['Chrome', 'Slack', 'Notes']);
    await refreshSourceApps();
    expect(sourceAppOptions.apps).toEqual(['Chrome', 'Slack', 'Notes']);
  });

  it('keeps the previous list when the fetch fails', async () => {
    sourceAppOptions.apps = ['Chrome'];
    vi.mocked(listSourceApps).mockRejectedValueOnce(new Error('backend gone'));
    await refreshSourceApps();
    expect(sourceAppOptions.apps).toEqual(['Chrome']);
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

  // Each preset anchors on local calendar midnights. Pinning the zone and the
  // clock lets every row name the exact instants it expects, including a
  // clock just past midnight (Tokyo, where UTC is still on the previous
  // day) and the 23- and 25-hour days around US daylight-saving switches.
  describe.each([
    {
      zone: 'UTC',
      now: '2026-05-08T15:30:00.000Z',
      today: '2026-05-08T00:00:00.000Z',
      yesterday: '2026-05-07T00:00:00.000Z',
      last7days: '2026-05-01T00:00:00.000Z',
      last30days: '2026-04-08T00:00:00.000Z',
    },
    {
      zone: 'Asia/Tokyo',
      now: '2026-05-08T15:30:00.000Z',
      today: '2026-05-08T15:00:00.000Z',
      yesterday: '2026-05-07T15:00:00.000Z',
      last7days: '2026-05-01T15:00:00.000Z',
      last30days: '2026-04-08T15:00:00.000Z',
    },
    {
      zone: 'America/New_York',
      // The day after clocks sprang forward: yesterday was 23 hours long.
      now: '2026-03-09T12:00:00.000Z',
      today: '2026-03-09T04:00:00.000Z',
      yesterday: '2026-03-08T05:00:00.000Z',
      last7days: '2026-03-02T05:00:00.000Z',
      last30days: '2026-02-07T05:00:00.000Z',
    },
    {
      zone: 'America/New_York',
      // The day after clocks fell back: yesterday was 25 hours long.
      now: '2026-11-02T12:00:00.000Z',
      today: '2026-11-02T05:00:00.000Z',
      yesterday: '2026-11-01T04:00:00.000Z',
      last7days: '2026-10-26T04:00:00.000Z',
      last30days: '2026-10-03T04:00:00.000Z',
    },
  ])('date presets in $zone at $now', ({ zone, now, today, yesterday, last7days, last30days }) => {
    let previousZone: string | undefined;

    beforeEach(() => {
      previousZone = process.env.TZ;
      // Node re-reads TZ on assignment for the whole process, which holds in
      // the default forks pool. Check it took effect so a runner that cannot
      // switch zones fails here rather than on a confusing date mismatch.
      process.env.TZ = zone;
      expect(Intl.DateTimeFormat().resolvedOptions().timeZone).toBe(zone);
      vi.useFakeTimers();
      vi.setSystemTime(new Date(now));
    });

    afterEach(() => {
      if (previousZone === undefined) delete process.env.TZ;
      else process.env.TZ = previousZone;
    });

    it('"today" starts at local midnight', () => {
      setDatePreset('today');
      expect(currentFilters()).toEqual({ createdAfter: today });
    });

    it('"yesterday" spans exactly the previous calendar day', () => {
      setDatePreset('yesterday');
      expect(currentFilters()).toEqual({ createdAfter: yesterday, createdBefore: today });
    });

    it('"last7days" starts at midnight seven calendar days back', () => {
      setDatePreset('last7days');
      expect(currentFilters()).toEqual({ createdAfter: last7days });
    });

    it('"last30days" starts at midnight thirty calendar days back', () => {
      setDatePreset('last30days');
      expect(currentFilters()).toEqual({ createdAfter: last30days });
    });
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

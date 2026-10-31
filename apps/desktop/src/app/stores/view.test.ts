import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

import { searchClipboard } from '../lib/commands';
import { resetSearchRuntimeForTest, scheduleQuery, searchState } from './searchQuery.svelte';
import { showPalette, showSettings, viewState } from './view.svelte';

beforeEach(() => {
  vi.useFakeTimers();
  searchState.query = '';
  searchState.appliedQuery = '';
  showPalette();
});

afterEach(() => {
  vi.useRealTimers();
  resetSearchRuntimeForTest();
});

describe('view store', () => {
  it('leaves a debounced search to fire while the palette stays up', async () => {
    vi.mocked(searchClipboard).mockResolvedValue({
      results: [],
      totalCandidates: 0,
      searchElapsedMs: 0,
      summaryElapsedMs: 0,
      totalElapsedMs: 0,
    });
    scheduleQuery('needle');
    showPalette();
    await vi.runAllTimersAsync();
    expect(searchClipboard).toHaveBeenCalledTimes(1);
  });

  it('switching to settings drops a debounced palette search', async () => {
    scheduleQuery('needle');
    showSettings();
    expect(viewState.current).toBe('settings');

    // The debounce would otherwise fire against the hidden palette.
    await vi.runAllTimersAsync();
    expect(searchClipboard).not.toHaveBeenCalled();

    showPalette();
    expect(viewState.current).toBe('palette');
  });
});

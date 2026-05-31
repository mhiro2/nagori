import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => false),
}));

vi.mock('../lib/commands', () => ({
  searchClipboard: vi.fn(),
}));

import type { SearchResultDto } from '../lib/types';
import { clearFilters, filterState } from '../stores/searchFilters.svelte';
import { searchState } from '../stores/searchQuery.svelte';
import FilterChips from './FilterChips.svelte';

const result = (id: string, sourceAppName?: string): SearchResultDto => ({
  id,
  kind: 'text',
  preview: id,
  score: 1,
  createdAt: new Date().toISOString(),
  pinned: false,
  sensitivity: 'Public',
  rankReasons: ['Recent'],
  ...(sourceAppName !== undefined ? { sourceAppName } : {}),
  representationSummary: [],
});

beforeEach(() => {
  clearFilters();
  searchState.query = '';
  searchState.results = [];
});

afterEach(cleanup);

describe('FilterChips', () => {
  it('renders the date, type, and pinned chips (no source apps yet)', () => {
    const { container } = render(FilterChips);
    const buttons = container.querySelectorAll('button');
    // 4 date presets + 5 kind chips + 1 pinned toggle.
    expect(buttons.length).toBe(10);
  });

  it('toggles pinnedOnly and reflects aria-pressed', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    const pinned = getByRole('button', { name: 'Pinned' });
    expect(pinned.getAttribute('aria-pressed')).toBe('false');

    await user.click(pinned);
    expect(filterState.pinnedOnly).toBe(true);
    expect(pinned.getAttribute('aria-pressed')).toBe('true');

    await user.click(pinned);
    expect(filterState.pinnedOnly).toBe(false);
  });

  it('switches the active date preset and toggles it off on re-click', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    await user.click(getByRole('button', { name: 'Today' }));
    expect(filterState.datePreset).toBe('today');
    await user.click(getByRole('button', { name: 'Last 7 days' }));
    expect(filterState.datePreset).toBe('last7days');
    await user.click(getByRole('button', { name: 'Last 7 days' }));
    expect(filterState.datePreset).toBe('none');
  });

  it('multi-selects content kinds', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    await user.click(getByRole('button', { name: 'URL' }));
    await user.click(getByRole('button', { name: 'Code' }));
    expect(filterState.kinds).toEqual(['url', 'code']);
    await user.click(getByRole('button', { name: 'URL' }));
    expect(filterState.kinds).toEqual(['code']);
  });

  it('derives source-app chips from the current result set', async () => {
    searchState.results = [
      result('a', 'Chrome'),
      result('b', 'Slack'),
      result('c', 'Chrome'),
      result('d'),
    ];
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    const chrome = getByRole('button', { name: 'Chrome' });
    const slack = getByRole('button', { name: 'Slack' });
    expect(chrome).toBeDefined();
    expect(slack).toBeDefined();

    await user.click(chrome);
    expect(filterState.sourceApp).toBe('Chrome');
  });
});

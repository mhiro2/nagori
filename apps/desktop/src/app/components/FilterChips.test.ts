import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => {
  const { tauriMock } = await import('../test-helpers/moduleMocks');
  return tauriMock({ isTauri: vi.fn(() => false) });
});

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

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
  it('renders the date pills, pinned toggle, and the type/app dropdowns', () => {
    const { container, getByRole } = render(FilterChips);
    // 4 date presets + 1 pinned toggle + 2 dropdown triggers (Type, App).
    // No source apps yet, so the App trigger is present but disabled, and no
    // clear button (nothing is active).
    expect(container.querySelectorAll('button').length).toBe(7);
    expect(getByRole('button', { name: 'Type' })).toBeDefined();
    expect(getByRole('button', { name: 'Source app' }).hasAttribute('disabled')).toBe(true);
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

  it('multi-selects content kinds from the type dropdown', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    // Open the Type dropdown; multi-select keeps it open between toggles.
    await user.click(getByRole('button', { name: 'Type' }));
    await user.click(getByRole('menuitemcheckbox', { name: 'URL' }));
    await user.click(getByRole('menuitemcheckbox', { name: 'Code' }));
    expect(filterState.kinds).toEqual(['url', 'code']);
    await user.click(getByRole('menuitemcheckbox', { name: 'URL' }));
    expect(filterState.kinds).toEqual(['code']);
  });

  it('single-selects a source app from the app dropdown', async () => {
    searchState.results = [
      result('a', 'Chrome'),
      result('b', 'Slack'),
      result('c', 'Chrome'),
      result('d'),
    ];
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    await user.click(getByRole('button', { name: 'Source app' }));
    expect(getByRole('menuitemradio', { name: 'Slack' })).toBeDefined();
    await user.click(getByRole('menuitemradio', { name: 'Chrome' }));
    expect(filterState.sourceApp).toBe('Chrome');
  });

  it('clears the source app via the "All apps" option', async () => {
    searchState.results = [result('a', 'Chrome'), result('b', 'Slack')];
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    await user.click(getByRole('button', { name: 'Source app' }));
    await user.click(getByRole('menuitemradio', { name: 'Chrome' }));
    expect(filterState.sourceApp).toBe('Chrome');
    // The trigger now announces the active value; re-open and pick "All apps".
    await user.click(getByRole('button', { name: 'Source app: Chrome' }));
    await user.click(getByRole('menuitemradio', { name: 'All apps' }));
    expect(filterState.sourceApp).toBeUndefined();
  });

  it('keeps the source app selected when its row is re-clicked (no toggle-off)', async () => {
    searchState.results = [result('a', 'Chrome'), result('b', 'Slack')];
    const user = userEvent.setup();
    const { getByRole } = render(FilterChips);
    await user.click(getByRole('button', { name: 'Source app' }));
    await user.click(getByRole('menuitemradio', { name: 'Chrome' }));
    expect(filterState.sourceApp).toBe('Chrome');
    // Re-opening and clicking the active row must NOT clear it — that's what
    // the "All apps" row is for now.
    await user.click(getByRole('button', { name: 'Source app: Chrome' }));
    await user.click(getByRole('menuitemradio', { name: 'Chrome' }));
    expect(filterState.sourceApp).toBe('Chrome');
  });

  it('shows a clear button only while a filter is active and resets on click', async () => {
    const user = userEvent.setup();
    const { getByRole, queryByRole } = render(FilterChips);
    expect(queryByRole('button', { name: 'Clear filters' })).toBeNull();

    await user.click(getByRole('button', { name: 'Pinned' }));
    const clear = getByRole('button', { name: 'Clear filters' });
    expect(clear).toBeDefined();

    await user.click(clear);
    expect(filterState.pinnedOnly).toBe(false);
    expect(queryByRole('button', { name: 'Clear filters' })).toBeNull();
  });
});

import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => false),
}));

vi.mock('../lib/commands', () => ({
  searchClipboard: vi.fn(),
}));

import { clearFilterPreset, filterState } from '../stores/searchFilters.svelte';
import { searchState } from '../stores/searchQuery.svelte';
import FilterChips from './FilterChips.svelte';

beforeEach(() => {
  clearFilterPreset();
  searchState.query = '';
  searchState.results = [];
});

afterEach(cleanup);

describe('FilterChips', () => {
  it('renders the three preset chips', () => {
    const { container } = render(FilterChips);
    const buttons = container.querySelectorAll('button');
    expect(buttons.length).toBe(3);
  });

  it('marks the active preset with aria-pressed=true and toggles off on a second click', async () => {
    const user = userEvent.setup();
    const { container } = render(FilterChips);
    const buttons = Array.from(container.querySelectorAll('button'));
    const pinned = buttons[2];
    expect(pinned).toBeDefined();
    expect(pinned?.getAttribute('aria-pressed')).toBe('false');

    await user.click(pinned!);
    expect(filterState.preset).toBe('pinned');

    await user.click(pinned!);
    expect(filterState.preset).toBe('none');
  });

  it('switches between presets when a different chip is clicked', async () => {
    const user = userEvent.setup();
    const { container } = render(FilterChips);
    const buttons = Array.from(container.querySelectorAll('button'));
    await user.click(buttons[0]!); // today
    expect(filterState.preset).toBe('today');
    await user.click(buttons[1]!); // last7days
    expect(filterState.preset).toBe('last7days');
  });
});

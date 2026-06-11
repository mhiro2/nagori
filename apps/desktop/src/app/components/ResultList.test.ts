import { cleanup, fireEvent, render, within } from '@testing-library/svelte';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

import type { SearchResultDto } from '../lib/types';
import { sampleSearchResult } from '../test-helpers/fixtures';
import ResultList from './ResultList.svelte';

beforeAll(() => {
  // jsdom does not implement Element.scrollIntoView; the list runs it inside
  // a $effect to keep the active row visible during keyboard navigation.
  Element.prototype.scrollIntoView = function () {};
});

const sample = (overrides: Partial<SearchResultDto> = {}): SearchResultDto =>
  sampleSearchResult({ id: 'id-1', preview: 'value', rankReasons: [], ...overrides });

afterEach(cleanup);

describe('ResultList', () => {
  it('renders the empty hint when items is empty', () => {
    const { getByRole, queryAllByRole } = render(ResultList, {
      props: {
        items: [],
        selectedIndex: 0,
        onSelect: () => {},
        onConfirm: () => {},
        emptyMessage: 'Nothing to see',
      },
    });

    const list = getByRole('listbox');
    expect(within(list).getByText('Nothing to see')).toBeTruthy();
    expect(queryAllByRole('option')).toHaveLength(0);
  });

  it('renders an option per item with the matching preview', () => {
    const items = [
      sample({ id: 'a', preview: 'alpha' }),
      sample({ id: 'b', preview: 'bravo' }),
      sample({ id: 'c', preview: 'charlie' }),
    ];

    const { getAllByRole, getByText } = render(ResultList, {
      props: { items, selectedIndex: 1, onSelect: () => {}, onConfirm: () => {} },
    });

    const options = getAllByRole('option');
    expect(options).toHaveLength(3);
    expect(getByText('alpha')).toBeTruthy();
    expect(getByText('bravo')).toBeTruthy();
    expect(getByText('charlie')).toBeTruthy();
  });

  it('marks the selectedIndex item with the .selected class', () => {
    const items = [sample({ id: 'a' }), sample({ id: 'b' }), sample({ id: 'c' })];

    const { getAllByRole } = render(ResultList, {
      props: { items, selectedIndex: 2, onSelect: () => {}, onConfirm: () => {} },
    });

    const options = getAllByRole('option');
    expect(options[0]?.classList.contains('selected')).toBe(false);
    expect(options[1]?.classList.contains('selected')).toBe(false);
    expect(options[2]?.classList.contains('selected')).toBe(true);
  });

  it('forwards locked to every row for reference-mode styling', () => {
    const items = [sample({ id: 'a' }), sample({ id: 'b' })];
    const { container } = render(ResultList, {
      props: { items, selectedIndex: 0, locked: true, onSelect: () => {}, onConfirm: () => {} },
    });
    expect(container.querySelectorAll('.result-row.locked')).toHaveLength(2);
  });

  it('exposes a listbox containing options with aria-selected reflecting selectedIndex', () => {
    const items = [sample({ id: 'a' }), sample({ id: 'b' }), sample({ id: 'c' })];
    const { getByRole, getAllByRole } = render(ResultList, {
      props: { items, selectedIndex: 1, onSelect: () => {}, onConfirm: () => {} },
    });
    // Verifies the WAI-ARIA contract: a listbox MUST own role="option" children
    // and only the active row should report aria-selected="true". Without this
    // the screen-reader announces "button" instead of "option N of 3".
    expect(getByRole('listbox')).toBeTruthy();
    const options = getAllByRole('option');
    expect(options.map((o) => o.getAttribute('aria-selected'))).toEqual(['false', 'true', 'false']);
  });

  it('auto-scrolls for navigation and new queries but not same-query refreshes', async () => {
    const itemsA = [sample({ id: 'a' }), sample({ id: 'b' }), sample({ id: 'c' })];
    const spy = vi.spyOn(Element.prototype, 'scrollIntoView');
    const { rerender } = render(ResultList, {
      props: {
        items: itemsA,
        selectedIndex: 0,
        appliedQuery: 'q',
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    // Initial mount reads as a new query (undefined -> 'q'); ignore that run.
    spy.mockClear();

    // Navigation: same array reference, the cursor moved -> keep it visible.
    await rerender({
      items: itemsA,
      selectedIndex: 2,
      appliedQuery: 'q',
      onSelect: () => {},
      onConfirm: () => {},
    });
    expect(spy).toHaveBeenCalled();
    spy.mockClear();

    // Same-query refresh (pin/delete/clipboard): the array is replaced and the
    // cursor reset, but the query is unchanged -> leave the scroll position.
    await rerender({
      items: [sample({ id: 'a' }), sample({ id: 'b' }), sample({ id: 'c' })],
      selectedIndex: 0,
      appliedQuery: 'q',
      onSelect: () => {},
      onConfirm: () => {},
    });
    expect(spy).not.toHaveBeenCalled();
    spy.mockClear();

    // New query: jump to the top of the fresh result set.
    await rerender({
      items: [sample({ id: 'x' })],
      selectedIndex: 0,
      appliedQuery: 'qq',
      onSelect: () => {},
      onConfirm: () => {},
    });
    expect(spy).toHaveBeenCalled();
    spy.mockRestore();
  });

  it('keeps navigation and hover selection correct at 200 rows', async () => {
    // The list is reused for large result sets; rows carry
    // `content-visibility: auto` (see ARCHITECTURE.md §12) instead of being
    // windowed, so every row stays in the DOM and the scroll/selection logic
    // must remain correct at scale.
    const items = Array.from({ length: 200 }, (_, i) =>
      sample({ id: `id-${i}`, preview: `row ${i}` }),
    );
    const spy = vi.spyOn(Element.prototype, 'scrollIntoView');
    const onSelect = vi.fn();
    const { rerender, container, getAllByRole } = render(ResultList, {
      props: { items, selectedIndex: 0, appliedQuery: 'q', onSelect, onConfirm: () => {} },
    });
    expect(getAllByRole('option')).toHaveLength(200);
    spy.mockClear();

    // Arrow far down the list: same array, cursor moved -> scroll into view.
    await rerender({ items, selectedIndex: 150, appliedQuery: 'q', onSelect, onConfirm: () => {} });
    expect(spy).toHaveBeenCalled();
    expect(container.querySelector('[aria-selected="true"]')?.textContent).toContain('row 150');

    // Hovering any row (even far down) still drives selection through onSelect.
    await fireEvent.mouseEnter(getAllByRole('option')[180] as Element);
    expect(onSelect).toHaveBeenCalledWith(180);
    spy.mockRestore();
  });
});

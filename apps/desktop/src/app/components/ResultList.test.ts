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

const rows = (count: number): SearchResultDto[] =>
  Array.from({ length: count }, (_, i) => sample({ id: `id-${i}`, preview: `row ${i}` }));

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

  it('keeps the cursor and the multi-selection on separate attributes', () => {
    const items = [sample({ id: 'a' }), sample({ id: 'b' }), sample({ id: 'c' })];
    const { getByRole, getAllByRole } = render(ResultList, {
      props: {
        items,
        selectedIndex: 1,
        multiSelected: new Set(['c']),
        listboxId: 'results',
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    const listbox = getByRole('listbox', { name: 'Clipboard history' });
    expect(listbox.id).toBe('results');
    expect(listbox.getAttribute('aria-multiselectable')).toBe('true');
    const options = getAllByRole('option');
    // `aria-selected` reports the multi-selection only; the highlighted row is
    // the single tab stop (roving tabindex) and the combobox's active
    // descendant, so a screen reader never hears the cursor as "selected".
    expect(options.map((o) => o.getAttribute('aria-selected'))).toEqual(['false', 'false', 'true']);
    expect(options.map((o) => o.getAttribute('tabindex'))).toEqual(['-1', '0', '-1']);
    expect(options[1]?.id).toBe('result-option-b');
  });

  it('auto-scrolls for navigation, new queries, and a moved selection on refresh', async () => {
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

    // Same-query refresh (pin/delete/clipboard) that keeps the cursor on the
    // same row: the array is replaced but nothing moved -> leave the scroll
    // position.
    await rerender({
      items: [sample({ id: 'a' }), sample({ id: 'b' }), sample({ id: 'c' })],
      selectedIndex: 2,
      appliedQuery: 'q',
      onSelect: () => {},
      onConfirm: () => {},
    });
    expect(spy).not.toHaveBeenCalled();
    spy.mockClear();

    // Same-query refresh where a new capture pushed the selected entry down a
    // row -> keep the entry the next Enter acts on in view.
    await rerender({
      items: [sample({ id: 'n' }), sample({ id: 'a' }), sample({ id: 'b' }), sample({ id: 'c' })],
      selectedIndex: 3,
      appliedQuery: 'q',
      onSelect: () => {},
      onConfirm: () => {},
    });
    expect(spy).toHaveBeenCalled();
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
    expect(container.querySelector('.result-item.selected')?.textContent).toContain('row 150');

    // Hovering any row (even far down) still drives selection through onSelect.
    await fireEvent.mouseEnter(getAllByRole('option')[180] as Element);
    expect(onSelect).toHaveBeenCalledWith(180);
    spy.mockRestore();
  });

  it('offers to load more rows when the page filled up', async () => {
    const items = [sample({ id: 'a' }), sample({ id: 'b' })];
    const onLoadMore = vi.fn();
    const { getByRole, getByText } = render(ResultList, {
      props: {
        items,
        selectedIndex: 0,
        onSelect: () => {},
        onConfirm: () => {},
        canLoadMore: true,
        onLoadMore,
      },
    });
    expect(getByText('Showing the top 2 entries.')).toBeTruthy();
    // The footer sits outside the listbox so the listbox only owns options.
    const button = getByRole('button', { name: 'Show more' });
    expect(within(getByRole('listbox')).queryByRole('button', { name: 'Show more' })).toBeNull();
    await fireEvent.click(button);
    expect(onLoadMore).toHaveBeenCalledTimes(1);
  });

  it('suggests narrowing the search once the cap is reached', () => {
    const { getByText, queryByRole } = render(ResultList, {
      props: {
        items: [sample({ id: 'a' })],
        selectedIndex: 0,
        onSelect: () => {},
        onConfirm: () => {},
        limitReached: true,
      },
    });
    expect(getByText(/Refine the search or use filters/)).toBeTruthy();
    expect(queryByRole('button', { name: 'Show more' })).toBeNull();
  });

  it('hides the footer while every match fits on the page', () => {
    const { queryByText } = render(ResultList, {
      props: {
        items: [sample({ id: 'a' })],
        selectedIndex: 0,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(queryByText(/Showing the top/)).toBeNull();
  });

  it('moves focus to the first new row when the last page removes Show more', async () => {
    const base = { selectedIndex: 0, onSelect: () => {}, onConfirm: () => {} };
    const { getByRole, getAllByRole, rerender } = render(ResultList, {
      props: { ...base, items: rows(2), canLoadMore: true, onLoadMore: () => {} },
    });
    const button = getByRole('button', { name: 'Show more' });
    button.focus();
    await fireEvent.click(button);
    await rerender({ ...base, items: rows(3), canLoadMore: false, limitReached: false });
    expect(document.activeElement).toBe(getAllByRole('option')[2]);
  });

  it('announces the page count through a live region', () => {
    const { getByRole } = render(ResultList, {
      props: {
        items: [sample({ id: 'a' })],
        selectedIndex: 0,
        onSelect: () => {},
        onConfirm: () => {},
        canLoadMore: true,
      },
    });
    expect(getByRole('status').textContent).toContain('Showing the top 1 entries.');
  });

  it('focuses a new row even when the re-rank inserts it above the old end', async () => {
    const base = { selectedIndex: 0, onSelect: () => {}, onConfirm: () => {} };
    const before = [sample({ id: 'a' }), sample({ id: 'b' })];
    const { getByRole, getAllByRole, rerender } = render(ResultList, {
      props: { ...base, items: before, canLoadMore: true, onLoadMore: () => {} },
    });
    const button = getByRole('button', { name: 'Show more' });
    button.focus();
    await fireEvent.click(button);
    const after = [
      sample({ id: 'a' }),
      sample({ id: 'new', preview: 'fresh' }),
      sample({ id: 'b' }),
    ];
    await rerender({ ...base, items: after, canLoadMore: false, paged: true });
    expect(document.activeElement).toBe(getAllByRole('option')[1]);
  });

  it('announces the final count when the last page comes back short', () => {
    const { getByRole, queryByRole } = render(ResultList, {
      props: {
        items: [sample({ id: 'a' }), sample({ id: 'b' })],
        selectedIndex: 0,
        onSelect: () => {},
        onConfirm: () => {},
        paged: true,
      },
    });
    expect(getByRole('status').textContent).toContain('Showing all 2 entries.');
    expect(queryByRole('button', { name: 'Show more' })).toBeNull();
  });
});

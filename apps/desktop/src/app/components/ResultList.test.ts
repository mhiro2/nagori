import { cleanup, render, within } from '@testing-library/svelte';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';

import type { SearchResultDto } from '../lib/types';
import ResultList from './ResultList.svelte';

beforeAll(() => {
  // jsdom does not implement Element.scrollIntoView; the list runs it inside
  // a $effect to keep the active row visible during keyboard navigation.
  Element.prototype.scrollIntoView = function () {};
});

const sample = (overrides: Partial<SearchResultDto> = {}): SearchResultDto => ({
  id: 'id-1',
  kind: 'text',
  preview: 'value',
  score: 0,
  createdAt: '2026-05-05T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: [],
  ...overrides,
});

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
});

import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { SearchResultDto } from '../lib/types';
import ResultItem from './ResultItem.svelte';

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

describe('ResultItem', () => {
  it('forwards onConfirm with the row index when clicked', async () => {
    const onConfirm = vi.fn();
    const { getByRole } = render(ResultItem, {
      props: {
        item: sample({ id: 'a', preview: 'first' }),
        index: 7,
        selected: false,
        onSelect: () => {},
        onConfirm,
      },
    });
    await fireEvent.click(getByRole('option'));
    expect(onConfirm).toHaveBeenCalledWith(7);
  });

  it('calls onSelect on mouse-enter so the row tracks the cursor', async () => {
    const onSelect = vi.fn();
    const { getByRole } = render(ResultItem, {
      props: {
        item: sample({ id: 'a' }),
        index: 3,
        selected: false,
        onSelect,
        onConfirm: () => {},
      },
    });
    await fireEvent.mouseEnter(getByRole('option'));
    expect(onSelect).toHaveBeenCalledWith(3);
  });

  it('renders a domain + path split for url-kind entries', () => {
    const { getByText, container } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'url',
          preview: 'https://example.com/foo/bar?q=1',
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByText('example.com')).toBeTruthy();
    // Path + query are concatenated into a single span.
    expect(container.querySelector('.preview.url .path')?.textContent).toBe('/foo/bar?q=1');
  });

  it('falls back to the plain preview when the url is unparseable', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ kind: 'url', preview: 'not-a-url' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.preview.url')).toBeNull();
    expect(container.querySelector('.preview')?.textContent).toContain('not-a-url');
  });

  it('emits a code-language badge when the body looks like JSON', () => {
    const { getByText } = render(ResultItem, {
      props: {
        item: sample({ kind: 'code', preview: '{"hello": "world"}' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByText('JSON')).toBeTruthy();
  });

  it('annotates Secret sensitivity in the meta strip', () => {
    const { getByText } = render(ResultItem, {
      props: {
        item: sample({ sensitivity: 'Secret' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByText('Secret')).toBeTruthy();
  });

  it('renders a pin icon for pinned entries', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ pinned: true }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.pin[aria-label="pinned"]')).toBeTruthy();
  });

  it('applies the .selected class when selected is true', () => {
    const { getByRole } = render(ResultItem, {
      props: {
        item: sample(),
        index: 0,
        selected: true,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByRole('option').classList.contains('selected')).toBe(true);
  });
});

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
  representationSummary: [],
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
    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onConfirm.mock.calls[0]?.[0]).toBe(7);
    // Modifier-aware multi-select uses the second argument; just verify
    // the click forwards a MouseEvent so Palette can read metaKey/shiftKey.
    expect(onConfirm.mock.calls[0]?.[1]).toBeInstanceOf(MouseEvent);
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

  it('also selects the row when the cursor enters the pin column', async () => {
    // The pin button is a sibling of the row button, so without its own
    // mouse-enter, hovering the pin column would not select the row — leaving
    // the pin reveal keyed off a different (or no) selected row.
    const onSelect = vi.fn();
    const { container } = render(ResultItem, {
      props: {
        item: sample({ id: 'a' }),
        index: 6,
        selected: false,
        onSelect,
        onConfirm: () => {},
      },
    });
    const toggle = container.querySelector('.pin-toggle');
    expect(toggle).toBeTruthy();
    await fireEvent.mouseEnter(toggle as Element);
    expect(onSelect).toHaveBeenCalledWith(6);
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

  it('shows the strongest match reason as a chip for query-driven rows', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ rankReasons: ['SubstringMatch', 'PrefixMatch', 'ExactMatch'] }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    const chip = container.querySelector('.rank-chip');
    expect(chip?.textContent?.trim()).toBe('Exact');
    expect(chip?.getAttribute('data-reason')).toBe('ExactMatch');
  });

  it('omits the reason chip for recent-listing rows (boost-only reasons)', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ rankReasons: ['Recent'] }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.rank-chip')).toBeNull();
  });

  it('marks the pin toggle active and pressed for pinned entries', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ pinned: true }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    const toggle = container.querySelector('.pin-toggle');
    expect(toggle?.classList.contains('active')).toBe(true);
    expect(toggle?.getAttribute('aria-pressed')).toBe('true');
  });

  it('forwards onTogglePin with the row index without confirming the row', async () => {
    const onTogglePin = vi.fn();
    const onConfirm = vi.fn();
    const { container } = render(ResultItem, {
      props: {
        item: sample(),
        index: 4,
        selected: true,
        onSelect: () => {},
        onConfirm,
        onTogglePin,
      },
    });
    const toggle = container.querySelector('.pin-toggle');
    expect(toggle).toBeTruthy();
    await fireEvent.click(toggle as Element);
    expect(onTogglePin).toHaveBeenCalledWith(4);
    // The pin button is a sibling of the row button, so toggling pin must not
    // also fire the row's paste/confirm path.
    expect(onConfirm).not.toHaveBeenCalled();
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

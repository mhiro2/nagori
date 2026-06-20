import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { SearchResultDto } from '../lib/types';
import { sampleSearchResult } from '../test-helpers/fixtures';
import ResultItem from './ResultItem.svelte';

const sample = (overrides: Partial<SearchResultDto> = {}): SearchResultDto =>
  sampleSearchResult({ id: 'id-1', preview: 'value', rankReasons: [], ...overrides });

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

  it('prefers the backend-detected language for the code badge', () => {
    // When the daemon ships a canonical `language`, the badge uses it rather
    // than re-sniffing the preview text (here the preview would not sniff SQL).
    const { getByText } = render(ResultItem, {
      props: {
        item: sample({ kind: 'code', preview: 'value', language: 'sql' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByText('SQL')).toBeTruthy();
  });

  it('shows a strong-brand badge for known URL hosts', () => {
    const { getByTestId } = render(ResultItem, {
      props: {
        item: sample({ kind: 'url', preview: 'https://www.youtube.com/watch?v=abc' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByTestId('url-brand').textContent).toBe('YouTube');
  });

  it('omits the brand badge for unknown URL hosts', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ kind: 'url', preview: 'https://example.com/foo' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('[data-testid="url-brand"]')).toBeNull();
  });

  it('shows dimensions, size, and a screenshot badge for image rows', () => {
    const { getByTestId, getByText } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'image',
          preview: '',
          imageWidth: 1920,
          imageHeight: 1080,
          sourceAppName: 'CleanShot X',
          representationSummary: [{ mimeType: 'image/png', role: 'primary', byteCount: 2_400_000 }],
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByTestId('image-dims').textContent).toBe('1920×1080');
    expect(getByText('2.3 MB')).toBeTruthy();
    // Screenshot badge uses the localized label (English default in tests).
    expect(getByText('Screenshot')).toBeTruthy();
  });

  it('never shows a raw representation badge on the row, even with multiple formats', () => {
    // The row no longer dumps the MIME list ("HTML + Plain" / "PNG + Plain");
    // the kind badge already names the primary, and the extra formats fold into
    // the preview pane's Details as user-facing categories.
    for (const kind of ['text', 'image', 'fileList'] as const) {
      const { container, unmount } = render(ResultItem, {
        props: {
          item: sample({
            kind,
            representationSummary: [
              { mimeType: 'text/html', role: 'primary', byteCount: 40 },
              { mimeType: 'image/png', role: 'alternative', byteCount: 2000 },
              { mimeType: 'text/plain', role: 'plainFallback', byteCount: 20 },
            ],
          }),
          index: 0,
          selected: false,
          onSelect: () => {},
          onConfirm: () => {},
        },
      });
      expect(container.querySelector('.rep-badge')).toBeNull();
      unmount();
    }
  });

  it('degrades gracefully for image rows without dimensions', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'image',
          preview: '',
          sourceAppName: 'Safari',
          representationSummary: [{ mimeType: 'image/png', role: 'primary', byteCount: 512 }],
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    // No dimensions probed, not a screenshot source — only the size shows.
    expect(container.querySelector('[data-testid="image-dims"]')).toBeNull();
    expect(container.textContent).toContain('512 B');
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

  it('applies the .locked class for reference mode', () => {
    // The action inspector is open: the row carries `locked` so the recede/lift
    // styling applies. Whether hover actually re-selects is the palette's call,
    // not the row's — see the `onSelect` test below.
    const { getByRole } = render(ResultItem, {
      props: {
        item: sample(),
        index: 0,
        selected: true,
        locked: true,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByRole('option').closest('.result-row')?.classList.contains('locked')).toBe(true);
  });

  it('still forwards onSelect on mouse-enter while locked (the palette owns the gate)', async () => {
    const onSelect = vi.fn();
    const { getByRole } = render(ResultItem, {
      props: {
        item: sample(),
        index: 2,
        selected: false,
        locked: true,
        onSelect,
        onConfirm: () => {},
      },
    });
    await fireEvent.mouseEnter(getByRole('option'));
    expect(onSelect).toHaveBeenCalledWith(2);
  });

  it('marks the query match inside the row preview', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ preview: 'the needle in the haystack' }),
        index: 0,
        selected: false,
        query: 'needle',
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    const marks = container.querySelectorAll('.preview mark.match');
    expect(marks).toHaveLength(1);
    expect(marks[0]?.textContent).toBe('needle');
  });

  it('highlights the host and path of a url row', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ kind: 'url', preview: 'https://example.com/docs/needle' }),
        index: 0,
        selected: false,
        query: 'example needle',
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.preview.url .domain mark.match')?.textContent).toBe('example');
    expect(container.querySelector('.preview.url .path mark.match')?.textContent).toBe('needle');
  });

  it('renders no marks when there is no query', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({ preview: 'plain recent row' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('mark.match')).toBeNull();
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

  it('renders a single file basename-first with an extension badge and location', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'fileList',
          preview: '/Users/example/Acme/Reports/quarterly-report.pptx',
          fileSummary: {
            total: 1,
            representativeNames: ['quarterly-report.pptx'],
            commonParentDisplay: 'Acme/Reports',
          },
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    // The extension badge replaces the generic FILES kind-badge.
    expect(container.querySelector('.kind-badge')?.textContent).toBe('PPTX');
    expect(container.querySelector('.preview.file .names')?.textContent).toBe(
      'quarterly-report.pptx',
    );
    expect(container.querySelector('.preview.file .context')?.textContent).toBe('Acme/Reports');
  });

  it('falls back to a generic FILE badge for an unknown extension', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'fileList',
          preview: '/tmp/archive.weirdext',
          fileSummary: { total: 1, representativeNames: ['archive.weirdext'] },
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.kind-badge')?.textContent).toBe('FILE');
    expect(container.querySelector('.preview.file .context')).toBeNull();
  });

  it('summarizes multiple files with a count badge, +N, and a common parent', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'fileList',
          preview: '/Users/example/Acme/Reports/quarterly-report.pptx',
          fileSummary: {
            total: 3,
            representativeNames: ['quarterly-report.pptx', 'budget.xlsx'],
            commonParentDisplay: 'Acme/Reports',
          },
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.kind-badge')?.textContent).toBe('3');
    const names = container.querySelector('.preview.file .names')?.textContent ?? '';
    expect(names).toContain('quarterly-report.pptx');
    expect(names).toContain('budget.xlsx');
    expect(container.querySelector('.preview.file .more')?.textContent).toBe('+1');
    expect(container.querySelector('.preview.file .context')?.textContent).toBe('Acme/Reports');
  });

  it('shows a location count when the files span multiple folders', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'fileList',
          fileSummary: {
            total: 4,
            representativeNames: ['a.txt', 'b.txt'],
            locationCount: 3,
          },
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.kind-badge')?.textContent).toBe('4');
    expect(container.querySelector('.preview.file .context.locations')?.textContent).toBe(
      '3 locations',
    );
  });

  it('gives file rows a basename-first accessible name', () => {
    const { getByRole } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'fileList',
          fileSummary: {
            total: 3,
            representativeNames: ['quarterly-report.pptx', 'budget.xlsx'],
            commonParentDisplay: 'Acme/Reports',
          },
        }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(getByRole('option').getAttribute('aria-label')).toBe(
      '3 files: quarterly-report.pptx, budget.xlsx +1, in Acme/Reports',
    );
  });

  it('highlights the query inside the file basename and location', () => {
    const { container } = render(ResultItem, {
      props: {
        item: sample({
          kind: 'fileList',
          fileSummary: {
            total: 1,
            representativeNames: ['quarterly-report.pptx'],
            commonParentDisplay: 'Acme/Reports',
          },
        }),
        index: 0,
        selected: false,
        query: 'report acme',
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.preview.file .names mark.match')?.textContent).toBe('report');
    const contextMarks = [...container.querySelectorAll('.preview.file .context mark.match')].map(
      (mark) => mark.textContent,
    );
    expect(contextMarks).toContain('Acme');
  });

  it('falls back to the plain preview for a file row without a summary', () => {
    // Sensitive / un-hydrated file lists carry no summary; the row must render
    // the (already redacted) preview rather than the basename-first layout.
    const { container } = render(ResultItem, {
      props: {
        item: sample({ kind: 'fileList', preview: '/tmp/a.txt\n/tmp/b.txt' }),
        index: 0,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
      },
    });
    expect(container.querySelector('.preview.file')).toBeNull();
    expect(container.querySelector('.preview')?.textContent).toContain('/tmp/a.txt');
  });

  it('forwards onContextMenu with the row index on a right-click', async () => {
    const onContextMenu = vi.fn();
    const { getByRole } = render(ResultItem, {
      props: {
        item: sample(),
        index: 5,
        selected: false,
        onSelect: () => {},
        onConfirm: () => {},
        onContextMenu,
      },
    });
    await fireEvent.contextMenu(getByRole('option'));
    expect(onContextMenu).toHaveBeenCalledTimes(1);
    expect(onContextMenu.mock.calls[0]?.[0]).toBe(5);
    expect(onContextMenu.mock.calls[0]?.[1]).toBeInstanceOf(MouseEvent);
  });

  it('also forwards onContextMenu from the pin column so the whole row is covered', async () => {
    // The handler sits on the row wrapper, so a right-click anywhere on the row
    // — including the trailing pin column (its own sibling button) — bubbles up
    // and forwards the index.
    const onContextMenu = vi.fn();
    const { container } = render(ResultItem, {
      props: {
        item: sample(),
        index: 6,
        selected: true,
        onSelect: () => {},
        onConfirm: () => {},
        onContextMenu,
      },
    });
    const toggle = container.querySelector('.pin-toggle');
    expect(toggle).toBeTruthy();
    await fireEvent.contextMenu(toggle as Element);
    expect(onContextMenu).toHaveBeenCalledWith(6, expect.any(MouseEvent));
  });
});

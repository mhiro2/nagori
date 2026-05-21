import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { EntryPreviewDto, SearchResultDto } from '../lib/types';
import PreviewPane from './PreviewPane.svelte';
import { tokenize } from './tokenize';

const sampleItem = (overrides: Partial<SearchResultDto> = {}): SearchResultDto => ({
  id: 'entry-id',
  kind: 'text',
  preview: 'preview body',
  score: 0,
  createdAt: '2026-05-05T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: ['Recent'],
  representationSummary: [],
  ...overrides,
});

const samplePreview = (
  overrides: Partial<EntryPreviewDto> & { body?: EntryPreviewDto['body'] } = {},
): EntryPreviewDto => ({
  id: 'entry-id',
  kind: 'text',
  title: 'Title',
  previewText: 'preview body',
  body: { type: 'text', text: 'preview body' },
  metadata: {
    byteCount: 12,
    charCount: 12,
    lineCount: 1,
    truncated: false,
    sensitive: false,
    fullContentAvailable: true,
  },
  ...overrides,
});

afterEach(cleanup);

describe('tokenize', () => {
  it('emits a single text span for a plain identifier', () => {
    const tokens = tokenize('hello');
    expect(tokens).toEqual([{ kind: 'text', text: 'hello' }]);
  });

  it('classifies known keywords distinctly from regular identifiers', () => {
    const tokens = tokenize('let foo');
    expect(tokens.find((t) => t.text === 'let')?.kind).toBe('kw');
    expect(tokens.find((t) => t.text === 'foo')?.kind).toBe('text');
  });

  it('treats double-quoted strings as a single str span', () => {
    const tokens = tokenize('"hi"');
    expect(tokens).toEqual([{ kind: 'str', text: '"hi"' }]);
  });

  it('handles backtick template literals and escaped quotes', () => {
    expect(tokenize('`x`')[0]?.kind).toBe('str');
    const escaped = tokenize('"a\\"b"');
    expect(escaped[0]?.kind).toBe('str');
    expect(escaped[0]?.text).toBe('"a\\"b"');
  });

  it('captures // line comments to end-of-line', () => {
    const tokens = tokenize('// note\nfn');
    expect(tokens[0]).toEqual({ kind: 'comment', text: '// note' });
  });

  it('captures # comments only when they start a line', () => {
    const tokens = tokenize('# header');
    expect(tokens[0]?.kind).toBe('comment');
    // mid-line `#` should not start a comment.
    const inline = tokenize('let # bad');
    expect(inline.some((t) => t.kind === 'comment')).toBe(false);
  });

  it('classifies numeric literals including separators', () => {
    expect(tokenize('1_000')[0]).toEqual({ kind: 'num', text: '1_000' });
    expect(tokenize('3.14')[0]).toEqual({ kind: 'num', text: '3.14' });
  });

  it('emits punct tokens for delimiters', () => {
    const tokens = tokenize('a+b');
    expect(tokens.map((t) => t.kind)).toEqual(['text', 'punct', 'text']);
  });
});

describe('PreviewPane', () => {
  beforeEach(() => {
    vi.stubGlobal('navigator', { userAgent: 'jsdom' });
  });

  it('renders the empty hint when no item is selected', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: undefined,
        preview: undefined,
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('.empty')).toBeTruthy();
  });

  it('renders the loading state when loading and no preview yet', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: undefined,
        loading: true,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('.state')).toBeTruthy();
  });

  it('surfaces an error state', () => {
    const { getByText } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: undefined,
        loading: false,
        errorMessage: 'preview unavailable',
      },
    });
    expect(getByText('preview unavailable')).toBeTruthy();
  });

  it('renders the preview body for plain text', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview(),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('pre.body')?.textContent).toContain('preview body');
  });

  it('shows a head summary chip with line count and byte count for text bodies', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          previewText: 'a\nb\nc',
          body: { type: 'text', text: 'a\nb\nc' },
          metadata: {
            byteCount: 2048,
            charCount: 5,
            lineCount: 3,
            truncated: false,
            sensitive: false,
            fullContentAvailable: true,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const summary = container.querySelector('[data-testid="preview-summary"]')?.textContent ?? '';
    expect(summary).toMatch(/3/);
    expect(summary).toMatch(/KB|kB/i);
  });

  it('renders highlighted code via tokenize for code/url body types', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'code' }),
        preview: samplePreview({
          previewText: 'let x = 1',
          body: { type: 'code', text: 'let x = 1' },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    // Code body produces inner spans for keyword/punct tokens.
    expect(container.querySelector('pre.body.code .kw')).toBeTruthy();
  });

  it('renders the file list paths when body is fileList', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'fileList', paths: ['/tmp/a.txt', '/tmp/b.txt'], total: 2 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const items = Array.from(container.querySelectorAll('ul.files > li'));
    expect(items).toHaveLength(2);
    expect(items[0]?.textContent).toContain('/tmp/');
    expect(items[0]?.textContent).toContain('a.txt');
    // Full path lives in `title` for hover-disclosure of middle-elided rows.
    expect(items[0]?.getAttribute('title')).toBe('/tmp/a.txt');
    // The basename is emphasised via <strong>; the directory part lives in
    // a dimmed span so the eye lands on the filename first.
    expect(items[0]?.querySelector('strong.base')?.textContent).toBe('a.txt');
    expect(items[0]?.querySelector('span.dim')?.textContent).toBe('/tmp/');
  });

  it('renders a "more files" hint when the file list exceeds the wire cap', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'fileList',
            paths: Array.from({ length: 50 }, (_, i) => `/tmp/file-${i}.txt`),
            total: 218,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const lis = Array.from(container.querySelectorAll('ul.files > li'));
    expect(lis).toHaveLength(51);
    expect(lis[lis.length - 1]?.classList.contains('more')).toBe(true);
    // The summary chip surfaces the truncated/total ratio for the kind.
    const summary = container.querySelector('[data-testid="preview-summary"]')?.textContent ?? '';
    expect(summary).toContain('50');
    expect(summary).toContain('218');
  });

  it('renders an <img> with the platform-default nagori-image:// URL', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'image' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'image', byteCount: 100 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const img = container.querySelector('img.image');
    expect(img?.getAttribute('src')).toMatch(/nagori-image:\/\/localhost\/entry-id$/);
    // Lazy load + async decode keep the image off the initial render's
    // critical path. The alt is a fixed, body-free description.
    expect(img?.getAttribute('loading')).toBe('lazy');
    expect(img?.getAttribute('decoding')).toBe('async');
    expect(img?.getAttribute('alt')).toBeTruthy();
    expect(img?.getAttribute('alt')).not.toBe('');
  });

  it('falls back to the unavailable state when the <img> errors', async () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'image' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'image', byteCount: 100 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const img = container.querySelector('img.image');
    expect(img).toBeTruthy();
    img?.dispatchEvent(new Event('error'));
    // Svelte 5 flushes effect-derived DOM updates on the microtask queue.
    await Promise.resolve();
    expect(container.querySelector('img.image')).toBeNull();
    expect(container.querySelector('.state')?.textContent ?? '').toMatch(
      /unavailable|not available/i,
    );
  });

  it('uses a body-free alt text on the <img> regardless of previewText', () => {
    const leak = 'SECRET-API-KEY-DO-NOT-LEAK';
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'image' }),
        preview: samplePreview({
          previewText: leak,
          body: { type: 'image', byteCount: 100 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const alt = container.querySelector('img.image')?.getAttribute('alt') ?? '';
    expect(alt).not.toContain(leak);
  });

  it('pins the <img> intrinsic size when width/height are known to avoid layout shift', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'image' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'image',
            byteCount: 1234567,
            mimeType: 'image/png',
            width: 1920,
            height: 1080,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const img = container.querySelector('img.image');
    expect(img?.getAttribute('width')).toBe('1920');
    expect(img?.getAttribute('height')).toBe('1080');
    // Skeleton stays in place until the onload handler flips the frame.
    expect(container.querySelector('.image-frame.loaded')).toBeNull();
    // The summary chip surfaces dimensions and MIME upper-cased.
    const summary = container.querySelector('[data-testid="preview-summary"]')?.textContent ?? '';
    expect(summary).toContain('1920×1080');
    expect(summary).toContain('PNG');
  });

  it('uses the http://nagori-image.localhost URL on Windows', () => {
    vi.stubGlobal('navigator', {
      userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
    });
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'image' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'image', byteCount: 100 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('img.image')?.getAttribute('src')).toMatch(
      /^http:\/\/nagori-image\.localhost\//,
    );
  });

  it('renders the truncated note when metadata flags it', () => {
    const { getByText } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          metadata: {
            byteCount: 1,
            charCount: 1,
            lineCount: 1,
            truncated: true,
            sensitive: false,
            fullContentAvailable: false,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    // Truncation label comes from the i18n dictionary; we only assert the
    // class hook that renders it.
    expect(getByText(/truncat/i)).toBeTruthy();
  });

  it('lists the source app and rank reasons in the footer when available', () => {
    const { getByText, container } = render(PreviewPane, {
      props: {
        item: sampleItem({ sourceAppName: 'Safari', rankReasons: ['ExactMatch', 'Recent'] }),
        preview: samplePreview(),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(getByText('Safari')).toBeTruthy();
    expect(container.querySelector('.foot')?.textContent).toMatch(/ExactMatch/);
  });

  it('shows a preserved-formats row when the entry kept multiple representations', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({
          representationSummary: [
            { mimeType: 'text/plain', role: 'primary', byteCount: 5 },
            { mimeType: 'text/html', role: 'alternative', byteCount: 20 },
          ],
        }),
        preview: samplePreview(),
        loading: false,
        errorMessage: undefined,
      },
    });
    const foot = container.querySelector('.foot')?.textContent ?? '';
    expect(foot).toMatch(/Plain.*HTML|HTML.*Plain/);
  });

  it('omits the preserved-formats row for single-format entries', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({
          representationSummary: [{ mimeType: 'text/plain', role: 'primary', byteCount: 5 }],
        }),
        preview: samplePreview(),
        loading: false,
        errorMessage: undefined,
      },
    });
    // The dt label for the formats row is keyed off t.preview.fields.formats
    // ("preserved formats" in en); it must not appear when there's only one.
    expect(container.querySelector('.foot')?.textContent).not.toMatch(/preserved formats/);
  });
});

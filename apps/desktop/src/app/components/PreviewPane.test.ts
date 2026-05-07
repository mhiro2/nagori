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
    const { getByText } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'fileList', paths: ['/tmp/a.txt', '/tmp/b.txt'] },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(getByText('/tmp/a.txt')).toBeTruthy();
    expect(getByText('/tmp/b.txt')).toBeTruthy();
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
});

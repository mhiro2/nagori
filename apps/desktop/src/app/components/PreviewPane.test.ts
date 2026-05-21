import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { EntryPreviewDto, SearchResultDto } from '../lib/types';
import PreviewPane from './PreviewPane.svelte';

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

  it('renders highlighted code via tokenize for code body type', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'code' }),
        preview: samplePreview({
          previewText: 'let x = 1',
          body: { type: 'code', text: 'let x = 1', language: 'typescript' },
          metadata: {
            byteCount: 9,
            charCount: 9,
            lineCount: 1,
            truncated: false,
            sensitive: false,
            fullContentAvailable: true,
            language: 'typescript',
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    // Code body produces inner spans for keyword/punct tokens.
    expect(container.querySelector('pre.body.code .kw')).toBeTruthy();
  });

  it('renders the URL kind with separate host and path rows, no code highlight', () => {
    // URL kind got its own layout (host on top, scheme+path below) so the
    // user can scan the destination at a glance and tell a punycode lookalike
    // host from a legitimate one before pressing Enter.
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'url', sensitivity: 'Public' }),
        preview: samplePreview({
          kind: 'url',
          previewText: 'https://example.com/foo?bar=1',
          body: {
            type: 'url',
            url: 'https://example.com/foo?bar=1',
            domain: 'example.com',
            scheme: 'https',
            hostDisplay: 'example.com',
            pathAndQuery: '/foo?bar=1',
          },
          metadata: {
            byteCount: 29,
            charCount: 29,
            lineCount: 1,
            truncated: false,
            sensitive: false,
            fullContentAvailable: true,
            domain: 'example.com',
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('[data-testid="preview-url-body"]')).toBeTruthy();
    expect(container.querySelector('[data-testid="preview-url-host"]')?.textContent).toContain(
      'example.com',
    );
    expect(container.querySelector('[data-testid="preview-url-path"]')?.textContent).toContain(
      '/foo?bar=1',
    );
    // The URL body must never reuse the code highlighter — the new layout
    // owns the visual treatment so we don't want the gutter / kw spans.
    expect(container.querySelector('pre.body.code')).toBeNull();
    // The punycode badge must stay hidden when the host already matches
    // the displayed Unicode form.
    expect(container.querySelector('[data-testid="preview-url-punycode-badge"]')).toBeNull();
  });

  it('shows the punycode badge when the host has an IDN ASCII mismatch', () => {
    // Phishing resistance: when the backend reports a different `hostPunycode`
    // than the Unicode `hostDisplay`, the renderer surfaces a status badge so
    // the user can spot homograph-style attacks before opening.
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'url', sensitivity: 'Public' }),
        preview: samplePreview({
          kind: 'url',
          previewText: 'https://bücher.example/foo',
          body: {
            type: 'url',
            url: 'https://bücher.example/foo',
            domain: 'bücher.example',
            scheme: 'https',
            hostDisplay: 'bücher.example',
            hostPunycode: 'xn--bcher-kva.example',
            pathAndQuery: '/foo',
          },
          metadata: {
            byteCount: 30,
            charCount: 27,
            lineCount: 1,
            truncated: false,
            sensitive: false,
            fullContentAvailable: true,
            domain: 'bücher.example',
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const badge = container.querySelector('[data-testid="preview-url-punycode-badge"]');
    expect(badge).toBeTruthy();
    // The badge's title carries the raw ASCII form so the user can
    // cross-check it against an external source without leaving the pane.
    expect(badge?.getAttribute('title')).toContain('xn--bcher-kva.example');
  });

  it('does not trigger an external open when Enter fires from the Cancel button', async () => {
    // Regression: the confirm dialog's overlay-level keydown handler used
    // to treat any Enter inside the dialog as confirmation, which would
    // open the URL even when the keyboard user had tabbed to Cancel. The
    // overlay now only fires `performOpenUrl()` when the dialog scaffold
    // itself owns focus; native button activation wins on the Cancel and
    // Open buttons.
    const invoke = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal('__TAURI_INTERNALS__', { invoke });
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'url', sensitivity: 'Public' }),
        preview: samplePreview({
          kind: 'url',
          previewText: 'https://example.com/',
          body: {
            type: 'url',
            url: 'https://example.com/',
            scheme: 'https',
            hostDisplay: 'example.com',
            pathAndQuery: '/',
          },
          metadata: {
            byteCount: 20,
            charCount: 20,
            lineCount: 1,
            truncated: false,
            sensitive: false,
            fullContentAvailable: true,
          },
        }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
      },
    });
    // Open the confirm dialog by clicking the trigger button; the
    // window-level Enter listener short-circuits while the dialog is
    // already open, so we have a clean baseline.
    const trigger = container.querySelector(
      '[data-testid="preview-url-open-button"]',
    ) as HTMLButtonElement | null;
    expect(trigger).not.toBeNull();
    trigger!.click();
    await Promise.resolve();
    const cancel = container.querySelector(
      '[data-testid="preview-url-confirm-cancel"]',
    ) as HTMLButtonElement | null;
    expect(cancel).not.toBeNull();
    cancel!.focus();
    cancel!.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }),
    );
    await Promise.resolve();
    expect(invoke).not.toHaveBeenCalledWith('open_url_external', expect.anything());
  });

  it('hides the open trigger and Enter hint for non-Public URL entries', () => {
    // Sensitivity gate must trip before any external-open affordance ships:
    // a Private URL shouldn't even let the user reach the confirm modal,
    // because the backend would reject the invoke and the user would just
    // see a forbidden toast.
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'url', sensitivity: 'Private' }),
        preview: samplePreview({
          kind: 'url',
          previewText: 'https://example.com/',
          body: {
            type: 'url',
            url: 'https://example.com/',
            scheme: 'https',
            hostDisplay: 'example.com',
            pathAndQuery: '/',
          },
          metadata: {
            byteCount: 20,
            charCount: 20,
            lineCount: 1,
            truncated: false,
            sensitive: true,
            fullContentAvailable: false,
          },
        }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
      },
    });
    expect(container.querySelector('[data-testid="preview-url-open-button"]')).toBeNull();
    expect(container.querySelector('[data-testid="preview-url-open-hint"]')).toBeNull();
  });

  it('wraps each code line and marks the line-number gutter aria-hidden', () => {
    const source = 'fn main() {\n    return 1;\n}\n';
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'code' }),
        preview: samplePreview({
          previewText: source,
          body: { type: 'code', text: source, language: 'rust' },
          metadata: {
            byteCount: source.length,
            charCount: source.length,
            lineCount: 3,
            truncated: false,
            sensitive: false,
            fullContentAvailable: true,
            language: 'rust',
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const pre = container.querySelector('pre.body.code.with-lines');
    expect(pre).toBeTruthy();
    const lines = pre?.querySelectorAll('span.line');
    expect(lines?.length).toBeGreaterThanOrEqual(3);
    // Every gutter element must carry aria-hidden so screen readers read
    // the source itself rather than the line-number column.
    const gutters = pre?.querySelectorAll('span.lineno');
    expect(gutters?.length).toBe(lines?.length);
    gutters?.forEach((g) => {
      expect(g.getAttribute('aria-hidden')).toBe('true');
    });
  });

  it('renders the file list paths when body is fileList', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'fileList',
            paths: ['/tmp/proj/a.txt', '/tmp/other/b.txt'],
            total: 2,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const items = Array.from(container.querySelectorAll('ul.files > li'));
    expect(items).toHaveLength(2);
    // Title retains the full original path for hover disclosure even when
    // the common parent prefix is stripped from the visible row.
    expect(items[0]?.getAttribute('title')).toBe('/tmp/proj/a.txt');
    // The basename is emphasised via <strong>; the directory part lives in
    // a dimmed span so the eye lands on the filename first.
    expect(items[0]?.querySelector('strong.base')?.textContent).toBe('a.txt');
    // The common parent `/tmp/` is hoisted, leaving `proj/` as the dim dir.
    expect(items[0]?.querySelector('span.dim')?.textContent).toBe('proj/');
  });

  it('hoists the common directory prefix into a single header above the list', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'fileList',
            paths: ['/Users/me/proj/a.txt', '/Users/me/proj/b.txt'],
            total: 2,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const header = container.querySelector('[data-testid="preview-files-common-parent"]');
    expect(header?.textContent).toContain('/Users/me/proj/');
    // Title preserves the full prefix so hover-discloses the path even if
    // CSS ellipsis truncates the visible header.
    expect(header?.getAttribute('title')).toBe('/Users/me/proj/');
    const items = Array.from(container.querySelectorAll('ul.files > li'));
    // With the common parent hoisted, each row collapses to the basename
    // only — no dir segment remains.
    expect(items[0]?.querySelector('span.dim')).toBeNull();
    expect(items[0]?.querySelector('strong.base')?.textContent).toBe('a.txt');
  });

  it('omits the common-parent header when there is only a single path', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'fileList', paths: ['/tmp/only.txt'], total: 1 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('[data-testid="preview-files-common-parent"]')).toBeNull();
    expect(container.querySelector('ul.files > li span.dim')?.textContent).toBe('/tmp/');
  });

  it('omits the common-parent header when the only shared prefix is the root', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'fileList', paths: ['/a.txt', '/b.txt'], total: 2 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    // `/` alone is too noisy to surface; the rows keep their own dir tokens.
    expect(container.querySelector('[data-testid="preview-files-common-parent"]')).toBeNull();
  });

  it('omits the common-parent header for a Windows drive root only', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'fileList', paths: ['C:\\a.txt', 'C:\\b.txt'], total: 2 },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('[data-testid="preview-files-common-parent"]')).toBeNull();
  });

  it('keeps a directory entry visible when it is also the common-parent input', () => {
    // When one of the paths is itself a directory (e.g. `/proj/build/`) and
    // a sibling lives inside it (`/proj/build/file.txt`), the common parent
    // is `/proj/` — the directory entry must still render as `build/` and
    // not collapse to an empty row.
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'fileList',
            paths: ['/proj/build/', '/proj/build/file.txt'],
            total: 2,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const header = container.querySelector('[data-testid="preview-files-common-parent"]');
    expect(header?.textContent).toContain('/proj/');
    const items = Array.from(container.querySelectorAll('ul.files > li'));
    expect(items[0]?.querySelector('strong.base')?.textContent).toBe('build/');
    expect(items[0]?.classList.contains('kind-directory')).toBe(true);
    expect(items[1]?.querySelector('span.dim')?.textContent).toBe('build/');
    expect(items[1]?.querySelector('strong.base')?.textContent).toBe('file.txt');
  });

  it('extracts the same common parent regardless of directory/file order', () => {
    // Order-independence guard: reversing the inputs must not pin the
    // prefix at `/proj/build/` and collapse the directory row.
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'fileList',
            paths: ['/proj/build/file.txt', '/proj/build/'],
            total: 2,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const header = container.querySelector('[data-testid="preview-files-common-parent"]');
    expect(header?.textContent).toContain('/proj/');
    const items = Array.from(container.querySelectorAll('ul.files > li'));
    expect(items[0]?.querySelector('span.dim')?.textContent).toBe('build/');
    expect(items[0]?.querySelector('strong.base')?.textContent).toBe('file.txt');
    expect(items[1]?.querySelector('strong.base')?.textContent).toBe('build/');
    expect(items[1]?.classList.contains('kind-directory')).toBe(true);
  });

  it('tags each row with an extension category for the colour dot', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'fileList',
            paths: [
              '/proj/photo.PNG',
              '/proj/main.rs',
              '/proj/release.zip',
              '/proj/spec.pdf',
              '/proj/Makefile',
              '/proj/build/',
            ],
            total: 6,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const items = Array.from(container.querySelectorAll('ul.files > li'));
    expect(items[0]?.classList.contains('kind-image')).toBe(true);
    expect(items[1]?.classList.contains('kind-code')).toBe(true);
    expect(items[2]?.classList.contains('kind-archive')).toBe(true);
    expect(items[3]?.classList.contains('kind-document')).toBe(true);
    expect(items[4]?.classList.contains('kind-unknown')).toBe(true);
    expect(items[5]?.classList.contains('kind-directory')).toBe(true);
    // The dot itself carries the category class so CSS can colour it
    // without the row's class hook needing global selectors.
    expect(items[0]?.querySelector('.ext-dot.image')).toBeTruthy();
    expect(items[5]?.querySelector('.ext-dot.directory')).toBeTruthy();
    // The colour dot must not be announced as content; aria-hidden keeps
    // screen readers focused on the path itself.
    expect(items[0]?.querySelector('.ext-dot')?.getAttribute('aria-hidden')).toBe('true');
  });

  it('re-attaches the trailing slash to directory rows so foo/ stays foo/', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'fileList' }),
        preview: samplePreview({
          previewText: '',
          body: {
            type: 'fileList',
            paths: ['/proj/build/', '/proj/dist/'],
            total: 2,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const items = Array.from(container.querySelectorAll('ul.files > li'));
    expect(items[0]?.querySelector('strong.base')?.textContent).toBe('build/');
    expect(items[1]?.querySelector('strong.base')?.textContent).toBe('dist/');
    expect(items[0]?.classList.contains('kind-directory')).toBe(true);
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

  it('renders the head+tail truncation note with elided byte count', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          metadata: {
            byteCount: 2_500_000,
            charCount: 2_500_000,
            lineCount: 4_200,
            truncated: true,
            sensitive: false,
            fullContentAvailable: true,
            truncation: { kind: 'headAndTail', elidedBytes: 2_500_000 - 128 * 1024 },
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    const note =
      container.querySelector('[data-testid="preview-truncation"] .note')?.textContent ?? '';
    // The headAndTail formatter mentions "first and last" + an elided byte count.
    expect(note).toMatch(/first and last/i);
    expect(note).toMatch(/MB|kB|KB/);
  });

  it('surfaces the elided-match warning when the query lives in the omitted middle', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          metadata: {
            byteCount: 2_500_000,
            charCount: 2_500_000,
            lineCount: 4_200,
            truncated: true,
            sensitive: false,
            fullContentAvailable: true,
            truncation: { kind: 'headAndTail', elidedBytes: 2_300_000 },
            elidedContainsMatch: true,
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('[data-testid="preview-elided-match"]')).toBeTruthy();
  });

  it('does not render the elided-match warning when no match in the middle', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          metadata: {
            byteCount: 2_500_000,
            charCount: 2_500_000,
            lineCount: 4_200,
            truncated: true,
            sensitive: false,
            fullContentAvailable: true,
            truncation: { kind: 'headAndTail', elidedBytes: 2_300_000 },
          },
        }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(container.querySelector('[data-testid="preview-elided-match"]')).toBeNull();
  });

  it('renders the expand button only when expanded mode is active and body is text-bearing', () => {
    const truncated = {
      byteCount: 2_500_000,
      charCount: 2_500_000,
      lineCount: 4_200,
      truncated: true,
      sensitive: false,
      fullContentAvailable: true,
      truncation: { kind: 'headAndTail' as const, elidedBytes: 2_300_000 },
    };
    // Compact (default) view hides the expand button even when eligible.
    const compact = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({ metadata: truncated }),
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(compact.container.querySelector('[data-testid="preview-expand-button"]')).toBeNull();
    cleanup();
    // Expanded view shows the button when the entry is Public + truncated + text-bearing.
    const expanded = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({ metadata: truncated }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
      },
    });
    expect(expanded.container.querySelector('[data-testid="preview-expand-button"]')).toBeTruthy();
  });

  it('hides the expand button when fullContentAvailable is false (Sensitive / non-Public)', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ sensitivity: 'Secret' }),
        preview: samplePreview({
          metadata: {
            byteCount: 2_500_000,
            charCount: 2_500_000,
            lineCount: 4_200,
            truncated: true,
            sensitive: true,
            fullContentAvailable: false,
            truncation: { kind: 'headAndTail', elidedBytes: 2_300_000 },
          },
        }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
      },
    });
    expect(container.querySelector('[data-testid="preview-expand-button"]')).toBeNull();
  });

  it('hides the expand button for non-text bodies even when truncated', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem({ kind: 'image' }),
        preview: samplePreview({
          previewText: '',
          body: { type: 'image', byteCount: 100 },
          metadata: {
            byteCount: 100,
            charCount: 0,
            lineCount: 0,
            truncated: true,
            sensitive: false,
            fullContentAvailable: true,
            truncation: { kind: 'headAndTail', elidedBytes: 64 },
          },
        }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
      },
    });
    expect(container.querySelector('[data-testid="preview-expand-button"]')).toBeNull();
  });

  it('disables the expand button while expansion is in flight', () => {
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          metadata: {
            byteCount: 2_500_000,
            charCount: 2_500_000,
            lineCount: 4_200,
            truncated: true,
            sensitive: false,
            fullContentAvailable: true,
            truncation: { kind: 'headAndTail', elidedBytes: 2_300_000 },
          },
        }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
        expandedLoading: true,
      },
    });
    const btn = container.querySelector(
      '[data-testid="preview-expand-button"]',
    ) as HTMLButtonElement | null;
    expect(btn).toBeTruthy();
    expect(btn?.disabled).toBe(true);
  });

  it('invokes onExpandBody with the entry id when the expand button is clicked', async () => {
    const onExpandBody = vi.fn();
    const { container } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          metadata: {
            byteCount: 2_500_000,
            charCount: 2_500_000,
            lineCount: 4_200,
            truncated: true,
            sensitive: false,
            fullContentAvailable: true,
            truncation: { kind: 'headAndTail', elidedBytes: 2_300_000 },
          },
        }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
        onExpandBody,
      },
    });
    const btn = container.querySelector(
      '[data-testid="preview-expand-button"]',
    ) as HTMLButtonElement | null;
    btn?.click();
    await Promise.resolve();
    expect(onExpandBody).toHaveBeenCalledWith('entry-id');
  });

  it('renders the expanded error message when expansion failed', () => {
    const { getByRole } = render(PreviewPane, {
      props: {
        item: sampleItem(),
        preview: samplePreview({
          metadata: {
            byteCount: 2_500_000,
            charCount: 2_500_000,
            lineCount: 4_200,
            truncated: true,
            sensitive: false,
            fullContentAvailable: true,
            truncation: { kind: 'headAndTail', elidedBytes: 2_300_000 },
          },
        }),
        loading: false,
        errorMessage: undefined,
        expanded: true,
        expandedErrorMessage: 'expansion blocked',
      },
    });
    expect(getByRole('alert').textContent).toMatch(/expansion blocked/);
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

import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());

vi.mock('../lib/commands', async () =>
  (await import('../test-helpers/moduleMocks')).commandsMock(),
);

import { getEntryPreview } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { EntryPreviewDto } from '../lib/types';
import { hydratePreview, previewState } from './searchPreview.svelte';

const preview = (id: string): EntryPreviewDto => ({
  id,
  kind: 'text',
  title: 'T',
  previewText: 'p',
  body: { type: 'text', text: 'p' },
  metadata: {
    byteCount: 1,
    charCount: 1,
    lineCount: 1,
    truncated: false,
    sensitive: false,
    fullContentAvailable: true,
  },
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  previewState.entryId = undefined;
  previewState.query = undefined;
  previewState.preview = undefined;
  previewState.loading = false;
  previewState.loadingVisible = false;
  previewState.errorMessage = undefined;
});

describe('hydratePreview', () => {
  it('clears state and skips IPC for an undefined entry id', async () => {
    await hydratePreview(undefined);
    expect(getEntryPreview).not.toHaveBeenCalled();
    expect(previewState.entryId).toBeUndefined();
    expect(previewState.loading).toBe(false);
  });

  it('skips IPC outside the Tauri runtime even with a real id', async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    await hydratePreview('e1');
    expect(getEntryPreview).not.toHaveBeenCalled();
    expect(previewState.entryId).toBe('e1');
  });

  it('hydrates preview state on a successful fetch', async () => {
    vi.mocked(getEntryPreview).mockResolvedValue(preview('e1'));
    await hydratePreview('e1');
    expect(getEntryPreview).toHaveBeenCalledWith('e1', undefined);
    expect(previewState.preview?.id).toBe('e1');
    expect(previewState.loading).toBe(false);
  });

  it('records an error when the IPC rejects', async () => {
    vi.mocked(getEntryPreview).mockRejectedValue(new Error('boom'));
    await hydratePreview('e2');
    expect(previewState.errorMessage).toBe('boom');
    expect(previewState.loading).toBe(false);
  });

  it('short-circuits when the same id is requested while a preview is in hand', async () => {
    vi.mocked(getEntryPreview).mockResolvedValue(preview('e3'));
    await hydratePreview('e3');
    vi.mocked(getEntryPreview).mockClear();
    await hydratePreview('e3');
    expect(getEntryPreview).not.toHaveBeenCalled();
  });

  it('refetches when only the query changes so the elided-match hint stays current', async () => {
    vi.mocked(getEntryPreview).mockResolvedValue(preview('e4'));
    await hydratePreview('e4', 'foo');
    expect(getEntryPreview).toHaveBeenCalledWith('e4', 'foo');
    vi.mocked(getEntryPreview).mockClear();
    await hydratePreview('e4', 'bar');
    expect(getEntryPreview).toHaveBeenCalledWith('e4', 'bar');
    expect(previewState.query).toBe('bar');
  });

  it('never shows the loading message when the fetch resolves before the delay', async () => {
    vi.mocked(getEntryPreview).mockResolvedValue(preview('e5'));
    await hydratePreview('e5');
    expect(previewState.loadingVisible).toBe(false);
  });

  it('surfaces the loading message once a fetch outlives the delay', async () => {
    vi.useFakeTimers();
    try {
      let resolveFetch!: (value: EntryPreviewDto) => void;
      vi.mocked(getEntryPreview).mockReturnValue(
        new Promise<EntryPreviewDto>((resolve) => {
          resolveFetch = resolve;
        }),
      );
      const hydrating = hydratePreview('e6');
      expect(previewState.loadingVisible).toBe(false);
      await vi.advanceTimersByTimeAsync(200);
      expect(previewState.loadingVisible).toBe(true);
      resolveFetch(preview('e6'));
      await hydrating;
      expect(previewState.loadingVisible).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('does not carry a visible loading message from a slow entry into a fast one', async () => {
    vi.useFakeTimers();
    try {
      let resolveSlow!: (value: EntryPreviewDto) => void;
      vi.mocked(getEntryPreview).mockReturnValueOnce(
        new Promise<EntryPreviewDto>((resolve) => {
          resolveSlow = resolve;
        }),
      );
      const slow = hydratePreview('e7');
      await vi.advanceTimersByTimeAsync(200);
      expect(previewState.loadingVisible).toBe(true);

      // Switch to a new entry that resolves well under the delay.
      vi.mocked(getEntryPreview).mockResolvedValueOnce(preview('e8'));
      const fast = hydratePreview('e8');
      expect(previewState.loadingVisible).toBe(false);
      await vi.advanceTimersByTimeAsync(50);
      await fast;
      expect(previewState.loadingVisible).toBe(false);

      // Drain the abandoned slow fetch so it doesn't leak across tests.
      resolveSlow(preview('e7'));
      await slow;
    } finally {
      vi.useRealTimers();
    }
  });
});

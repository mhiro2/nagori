import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  getEntryPreview: vi.fn(),
}));

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
  previewState.preview = undefined;
  previewState.loading = false;
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
    expect(getEntryPreview).toHaveBeenCalledWith('e1');
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
});

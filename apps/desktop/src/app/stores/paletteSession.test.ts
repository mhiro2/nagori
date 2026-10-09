import { afterEach, describe, expect, it, vi } from 'vitest';

import {
  resetPaletteSessionForTest,
  runSessionEndingAction,
  takeReopenMode,
} from './paletteSession.svelte';

afterEach(() => {
  resetPaletteSessionForTest();
  vi.restoreAllMocks();
});

describe('palette session', () => {
  it('resumes after a dismissal and starts fresh after an action', async () => {
    expect(takeReopenMode()).toBe('resume');
    await runSessionEndingAction(async () => undefined);
    expect(takeReopenMode()).toBe('fresh');
    // Consumed: the reopen after that one resumes again.
    expect(takeReopenMode()).toBe('resume');
  });

  it('keeps resuming when the action fails while the palette is still up', async () => {
    vi.spyOn(document, 'hasFocus').mockReturnValue(true);
    await expect(
      runSessionEndingAction(async () => {
        throw new Error('copy failed');
      }),
    ).rejects.toThrow('copy failed');
    expect(takeReopenMode()).toBe('resume');
  });

  it('still starts fresh when the failure arrives after the palette hid', async () => {
    vi.spyOn(document, 'hasFocus').mockReturnValue(false);
    await expect(
      runSessionEndingAction(async () => {
        throw new Error('auto-paste failed');
      }),
    ).rejects.toThrow('auto-paste failed');
    expect(takeReopenMode()).toBe('fresh');
  });
});

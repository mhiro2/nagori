import { afterEach, describe, expect, it } from 'vitest';

import {
  noteActionEndedAfterHide,
  resetPaletteSessionForTest,
  runSessionEndingAction,
  takeReopenMode,
} from './paletteSession.svelte';

afterEach(() => {
  resetPaletteSessionForTest();
});

describe('palette session', () => {
  it('resumes after a dismissal and starts fresh after an action', async () => {
    expect(takeReopenMode()).toBe('resume');
    await runSessionEndingAction(async () => undefined);
    expect(takeReopenMode()).toBe('fresh');
    // Consumed: the reopen after that one resumes again.
    expect(takeReopenMode()).toBe('resume');
  });

  it('keeps resuming when the action fails without the backend reporting a hide', async () => {
    await expect(
      runSessionEndingAction(async () => {
        throw new Error('copy failed');
      }),
    ).rejects.toThrow('copy failed');
    expect(takeReopenMode()).toBe('resume');
  });

  it('starts fresh when the backend reports a failure after the palette hid', async () => {
    await expect(
      runSessionEndingAction(async () => {
        // The `paste_failed` event lands before the command's rejection.
        noteActionEndedAfterHide();
        throw new Error('auto-paste failed');
      }),
    ).rejects.toThrow('auto-paste failed');
    expect(takeReopenMode()).toBe('fresh');
  });

  it('ignores a paste failure that no palette action is waiting on', () => {
    noteActionEndedAfterHide();
    expect(takeReopenMode()).toBe('resume');
  });
});

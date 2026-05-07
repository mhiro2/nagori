import { describe, expect, it } from 'vitest';

import { showPalette, showSettings, viewState } from './view.svelte';

describe('view store', () => {
  it('toggles between palette and settings via the imperative helpers', () => {
    showSettings();
    expect(viewState.current).toBe('settings');
    showPalette();
    expect(viewState.current).toBe('palette');
  });
});

// Top-level view switcher. The MVP UI is keyboard-driven and toggles between
// the palette (default) and settings.

import { cancelPendingQuery } from './searchQuery.svelte';

export type ViewName = 'palette' | 'settings';

export const viewState = $state<{ current: ViewName }>({ current: 'palette' });

export const showPalette = (): void => {
  viewState.current = 'palette';
};

export const showSettings = (): void => {
  // Drop any debounced palette query before swapping views — the timer
  // would otherwise fire against an unmounted palette and clobber the
  // shared `searchState`. Centralised here so every transition path
  // (palette `open-settings`, legacy `navigate` event, dev/test
  // fallbacks) gets the same scrub for free.
  cancelPendingQuery();
  viewState.current = 'settings';
};

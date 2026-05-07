// Top-level view switcher. The MVP UI is keyboard-driven and toggles between
// the palette (default) and settings.

export type ViewName = 'palette' | 'settings';

export const viewState = $state<{ current: ViewName }>({ current: 'palette' });

export const showPalette = (): void => {
  viewState.current = 'palette';
};

export const showSettings = (): void => {
  viewState.current = 'settings';
};

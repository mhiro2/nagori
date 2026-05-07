import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./lib/tauri', () => ({
  isTauri: vi.fn(() => false),
}));

vi.mock('./lib/commands', () => ({
  hidePalette: vi.fn(async () => undefined),
}));

// The App shell wires keybindings + window blur; the route children are out
// of scope for these tests and bring their own DOM dependencies, so stub the
// route components down to inert anchors.
vi.mock('./routes/PaletteRoute.svelte', async () => {
  const Stub = (await import('./test-helpers/StubComponent.svelte')).default;
  return { default: Stub };
});

vi.mock('./routes/SettingsRoute.svelte', async () => {
  const Stub = (await import('./test-helpers/StubComponent.svelte')).default;
  return { default: Stub };
});

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import App from './App.svelte';
import { hidePalette } from './lib/commands';
import { isTauri } from './lib/tauri';
import { showPalette, showSettings, viewState } from './stores/view.svelte';

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(false);
  showPalette();
});

afterEach(cleanup);

describe('App shell', () => {
  it('mounts the palette route by default', () => {
    const { container } = render(App);
    expect(container.querySelector('.app-shell')).toBeTruthy();
    expect(viewState.current).toBe('palette');
  });

  it('returns to the palette when Escape fires while on the settings route', async () => {
    showSettings();
    render(App);
    await fireEvent.keyDown(window, { key: 'Escape' });
    expect(viewState.current).toBe('palette');
  });

  it('invokes hidePalette on Escape when on the palette route inside Tauri', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    await fireEvent.keyDown(window, { key: 'Escape' });
    expect(hidePalette).toHaveBeenCalled();
  });

  it('skips the Escape handler when defaultPrevented is true', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    const event = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true });
    event.preventDefault();
    window.dispatchEvent(event);
    expect(hidePalette).not.toHaveBeenCalled();
  });

  it('hides the palette on window blur', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    await fireEvent.blur(window);
    expect(hidePalette).toHaveBeenCalled();
  });

  it('does not hide on blur when the user is on the settings route', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    showSettings();
    render(App);
    await fireEvent.blur(window);
    expect(hidePalette).not.toHaveBeenCalled();
  });

  it('detaches its window listeners on unmount', async () => {
    const { unmount } = render(App);
    unmount();
    // Once unmounted, blur should no longer reach the (now removed) handler.
    vi.mocked(isTauri).mockReturnValue(true);
    await fireEvent.blur(window);
    expect(hidePalette).not.toHaveBeenCalled();
  });
});

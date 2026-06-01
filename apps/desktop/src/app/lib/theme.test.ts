import { beforeEach, describe, expect, it, vi } from 'vitest';

// Shared mock surface for the Tauri runtime. `vi.hoisted` keeps these defined
// before the hoisted `vi.mock` factories below capture them. `state.handler`
// records the `onThemeChanged` callback so a test can simulate an OS flip.
const mocks = vi.hoisted(() => {
  const state: { handler: ((event: { payload: 'light' | 'dark' }) => void) | undefined } = {
    handler: undefined,
  };
  return {
    isTauri: vi.fn(() => false),
    theme: vi.fn(async (): Promise<'light' | 'dark' | null> => null),
    onThemeChanged: vi.fn(async (handler: (event: { payload: 'light' | 'dark' }) => void) => {
      state.handler = handler;
      return () => {};
    }),
    state,
  };
});

vi.mock('./tauri', () => ({ isTauri: mocks.isTauri }));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ theme: mocks.theme, onThemeChanged: mocks.onThemeChanged }),
}));

// theme.ts holds per-webview module state (`desired`, `watching`), so reset
// the module registry between tests to start each one from a clean instance.
const importTheme = async (): Promise<typeof import('./theme')> => import('./theme');

const currentTheme = (): string | undefined => document.documentElement.dataset.theme;

beforeEach(() => {
  vi.resetModules();
  mocks.isTauri.mockReturnValue(false);
  mocks.theme.mockReset();
  mocks.theme.mockResolvedValue(null);
  mocks.onThemeChanged.mockClear();
  mocks.state.handler = undefined;
  document.documentElement.removeAttribute('data-theme');
});

describe('applyAppearance', () => {
  it('applies an explicit theme directly without consulting the OS', async () => {
    mocks.isTauri.mockReturnValue(true);
    const { applyAppearance } = await importTheme();

    applyAppearance('light');
    expect(currentTheme()).toBe('light');

    applyAppearance('dark');
    expect(currentTheme()).toBe('dark');

    expect(mocks.theme).not.toHaveBeenCalled();
  });

  it('falls back to the prefers-color-scheme media query in system mode outside Tauri', async () => {
    const { applyAppearance } = await importTheme();

    applyAppearance('system');

    expect(currentTheme()).toBe('system');
    expect(mocks.theme).not.toHaveBeenCalled();
  });

  it('pins the concrete OS theme reported by Tauri in system mode', async () => {
    mocks.isTauri.mockReturnValue(true);
    mocks.theme.mockResolvedValue('light');
    const { applyAppearance } = await importTheme();

    applyAppearance('system');
    // Synchronous first paint leans on the media query…
    expect(currentTheme()).toBe('system');
    // …then the resolved OS theme is pinned so a later switch still lands.
    await vi.waitFor(() => expect(currentTheme()).toBe('light'));
  });

  it('keeps the media-query fallback when Tauri reports no concrete theme', async () => {
    mocks.isTauri.mockReturnValue(true);
    mocks.theme.mockResolvedValue(null);
    const { applyAppearance } = await importTheme();

    applyAppearance('system');
    expect(currentTheme()).toBe('system');
    // Let the (resolved-null) probe settle; it must not pin a guessed theme.
    await vi.waitFor(() => expect(mocks.theme).toHaveBeenCalledTimes(1));
    await Promise.resolve();
    expect(currentTheme()).toBe('system');
  });

  it('does not let a stale theme() read override a newer OS theme change', async () => {
    mocks.isTauri.mockReturnValue(true);
    // Defer the probe so we can resolve it *after* an OS theme event lands.
    // The executor runs synchronously, so `resolveTheme` is assigned before use.
    let resolveTheme!: (value: 'light' | 'dark' | null) => void;
    mocks.theme.mockReturnValue(
      new Promise<'light' | 'dark' | null>((resolve) => {
        resolveTheme = resolve;
      }),
    );
    const { applyAppearance, watchSystemTheme } = await importTheme();

    watchSystemTheme();
    await vi.waitFor(() => expect(mocks.onThemeChanged).toHaveBeenCalledTimes(1));

    applyAppearance('system');
    await vi.waitFor(() => expect(mocks.theme).toHaveBeenCalledTimes(1));

    // OS flips to light before the probe resolves.
    mocks.state.handler?.({ payload: 'light' });
    expect(currentTheme()).toBe('light');

    // The now-stale probe resolves to the old value — it must not win.
    resolveTheme('dark');
    await Promise.resolve();
    await Promise.resolve();
    expect(currentTheme()).toBe('light');
  });
});

describe('watchSystemTheme', () => {
  it('re-applies system appearance when the OS theme changes', async () => {
    mocks.isTauri.mockReturnValue(true);
    const { watchSystemTheme } = await importTheme();

    watchSystemTheme();
    await vi.waitFor(() => expect(mocks.onThemeChanged).toHaveBeenCalledTimes(1));

    mocks.state.handler?.({ payload: 'light' });
    expect(currentTheme()).toBe('light');

    mocks.state.handler?.({ payload: 'dark' });
    expect(currentTheme()).toBe('dark');
  });

  it('ignores OS theme changes once an explicit theme is pinned', async () => {
    mocks.isTauri.mockReturnValue(true);
    const { applyAppearance, watchSystemTheme } = await importTheme();

    watchSystemTheme();
    await vi.waitFor(() => expect(mocks.onThemeChanged).toHaveBeenCalledTimes(1));

    applyAppearance('dark');
    mocks.state.handler?.({ payload: 'light' });

    expect(currentTheme()).toBe('dark');
  });

  it('does not register a listener outside Tauri', async () => {
    const { watchSystemTheme } = await importTheme();

    watchSystemTheme();

    expect(mocks.onThemeChanged).not.toHaveBeenCalled();
  });
});

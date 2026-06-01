import { isTauri } from './tauri';
import type { Appearance } from './types';

// The appearance the user picked for *this* webview. `system` defers to the
// OS theme, which we resolve through Tauri rather than the webview's
// `prefers-color-scheme` media query: WebView2 on Windows only samples that
// query when the webview is first created, so a later OS light/dark switch
// never reached a running app until it was restarted (`Window.theme()` /
// `onThemeChanged` read the live OS theme and fire on every switch). Each
// webview (palette, settings) runs its own module instance, so this stays
// window-local.
let desired: Appearance = 'system';
let watching = false;
// Bumped on every appearance decision (an `applyAppearance` call or an
// `onThemeChanged` event). A slow async `Window.theme()` read captures the
// token at dispatch and skips its write if a newer decision has since landed,
// so a stale read can't clobber a fresher OS-theme event.
let epoch = 0;

const setDataTheme = (value: string): void => {
  document.documentElement.dataset.theme = value;
};

export const applyAppearance = (appearance: Appearance): void => {
  desired = appearance;
  const token = ++epoch;

  // Explicit light/dark is a pure CSS attribute switch — no OS lookup needed.
  if (appearance !== 'system') {
    setDataTheme(appearance);
    return;
  }

  // Paint from the media query first (accurate when the webview was just
  // created), then pin the concrete OS theme reported by Tauri so a later
  // switch still lands on Windows, where the media query goes stale.
  setDataTheme('system');
  if (!isTauri()) return;
  void (async () => {
    try {
      const { getCurrentWindow } = await import('@tauri-apps/api/window');
      const theme = await getCurrentWindow().theme();
      // Skip if a newer decision superseded us, or if Tauri can't report a
      // concrete theme (`null` on e.g. macOS ≤ 10.13) — keep the media-query
      // fallback in that case rather than guessing.
      if (token === epoch && desired === 'system' && theme) setDataTheme(theme);
    } catch {
      // Theme probe failed — the media-query paint above still stands.
    }
  })();
};

// Re-resolve `system` appearance whenever the OS flips light/dark. Idempotent
// and attached once per webview; call it once at startup (see `main.ts`) so
// both the palette and the settings window track the OS independently.
export const watchSystemTheme = (): void => {
  if (watching || !isTauri()) return;
  watching = true;
  void (async () => {
    try {
      const { getCurrentWindow } = await import('@tauri-apps/api/window');
      await getCurrentWindow().onThemeChanged(({ payload }) => {
        if (desired !== 'system') return;
        // The event payload is the freshest signal — supersede any in-flight
        // `theme()` read so it can't overwrite this with a stale value.
        epoch++;
        setDataTheme(payload);
      });
    } catch {
      // Listener registration failed — `system` mode falls back to the
      // startup media-query paint until the next launch.
    }
  })();
};

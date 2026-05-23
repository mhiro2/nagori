<script lang="ts">
  import { onMount } from "svelte";
  import { hidePalette, openSettingsWindow } from "./lib/commands";
  import { messages } from "./lib/i18n/index.svelte";
  import { TAURI_EVENTS, currentWindowLabel, isTauri, subscribe } from "./lib/tauri";
  import PaletteRoute from "./routes/PaletteRoute.svelte";
  import SettingsRoute from "./routes/SettingsRoute.svelte";
  import { showPalette, showSettings, viewState } from "./stores/view.svelte";

  // Settings runs in its own native window (`label: "settings"` in
  // `tauri.conf.json`). The palette window keeps using the in-process
  // `viewState` toggle so dev/test contexts without Tauri still work.
  const isSettingsWindow = currentWindowLabel() === "settings";

  const t = $derived(messages());

  // Hide-on-blur and Esc are window-level concerns (the palette is the
  // window). Components can still consume Escape inside ActionMenu / inputs
  // and stop propagation; the global handler only fires for unhandled keys.
  // The Settings window owns its own lifecycle (OS title-bar close, Cmd+W
  // through native decorations), so these handlers only run for the
  // palette window.
  const handleEscape = (event: KeyboardEvent): void => {
    if (event.key !== "Escape" || event.defaultPrevented) return;
    if (viewState.current === "settings") {
      showPalette();
      return;
    }
    void hidePalette();
  };

  const handleBlur = (): void => {
    // When the user clicks away the palette is no longer useful — hide it so
    // the next hotkey press feels like a fresh invocation. Settings stays
    // visible because the user explicitly navigated there.
    if (viewState.current !== "palette") return;
    void hidePalette();
  };

  onMount(() => {
    if (isSettingsWindow) {
      // Surface paste-failed toasts even when only the Settings window is
      // foregrounded — the permission prompt link inside the toast lives
      // here. No Esc / blur wiring: the OS title bar drives close.
      const offPasteFailed = subscribe<{ error?: string }>(TAURI_EVENTS.pasteFailed, (payload) => {
        pasteFailureMessage = payload?.error ?? messages().toasts.autoPasteFailedFallback;
      });
      return () => {
        offPasteFailed();
      };
    }

    window.addEventListener("keydown", handleEscape);
    window.addEventListener("blur", handleBlur);

    // Legacy in-window navigation. The tray now opens Settings as a
    // standalone window via the `open_settings` IPC, but we keep this
    // handler so a future caller emitting the event still lands somewhere
    // reasonable (palette dev mode, non-Tauri tests).
    const offNavigate = subscribe<string>(TAURI_EVENTS.navigate, (payload) => {
      if (payload === "settings") showSettings();
      else if (payload === "palette") showPalette();
    });
    // Accessibility loss / paste failure: surface a toast that nudges the
    // user into Settings, where they can re-grant the permission.
    const offPasteFailed = subscribe<{ error?: string }>(TAURI_EVENTS.pasteFailed, (payload) => {
      pasteFailureMessage = payload?.error ?? messages().toasts.autoPasteFailedFallback;
    });

    return () => {
      window.removeEventListener("keydown", handleEscape);
      window.removeEventListener("blur", handleBlur);
      offNavigate();
      offPasteFailed();
    };
  });

  let pasteFailureMessage = $state<string | null>(null);

  const dismissToast = (): void => {
    pasteFailureMessage = null;
  };

  const openSettingsFromToast = (): void => {
    if (isTauri()) {
      void openSettingsWindow();
    } else {
      showSettings();
    }
    pasteFailureMessage = null;
  };
</script>

<main class="app-shell" class:settings-window={isSettingsWindow}>
  {#if isSettingsWindow}
    <SettingsRoute />
  {:else if viewState.current === "palette"}
    <PaletteRoute />
  {:else}
    <SettingsRoute />
  {/if}
  {#if pasteFailureMessage}
    <div class="toast" role="status">
      <div class="toast-body">
        <strong>{t.toasts.autoPasteFailedTitle}</strong>
        <span class="toast-detail">{pasteFailureMessage}</span>
      </div>
      <div class="toast-actions">
        <button type="button" onclick={openSettingsFromToast}>{t.toasts.openSettings}</button>
        <button type="button" onclick={dismissToast}>{t.toasts.dismiss}</button>
      </div>
    </div>
  {/if}
</main>

<style>
  .app-shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
    background: var(--bg, #14161a);
    border-radius: 12px;
    position: relative;
  }
  /* The settings window uses the OS-native frame (decorations + opaque
     background), so the palette's rounded translucent shell would clip
     the title bar and leak the window background through the corners. */
  .app-shell.settings-window {
    border-radius: 0;
  }
  .toast {
    position: absolute;
    bottom: 0.75rem;
    right: 0.75rem;
    max-width: 22rem;
    padding: 0.6rem 0.75rem;
    border-radius: 8px;
    background: rgba(40, 16, 16, 0.92);
    color: #ffd9d9;
    border: 1px solid rgba(255, 100, 100, 0.5);
    font-size: 0.875rem;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.35);
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  .toast-body {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .toast-detail {
    color: rgba(255, 217, 217, 0.85);
    word-break: break-word;
  }
  .toast-actions {
    display: flex;
    gap: 0.4rem;
    justify-content: flex-end;
  }
  .toast-actions button {
    background: rgba(255, 255, 255, 0.08);
    color: inherit;
    border: 1px solid rgba(255, 255, 255, 0.18);
    border-radius: 4px;
    padding: 0.25rem 0.5rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .toast-actions button:hover {
    background: rgba(255, 255, 255, 0.16);
  }
</style>

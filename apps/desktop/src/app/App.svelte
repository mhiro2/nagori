<script lang="ts">
  import { onMount } from "svelte";
  import { hidePalette } from "./lib/commands";
  import { isTauri } from "./lib/tauri";
  import PaletteRoute from "./routes/PaletteRoute.svelte";
  import SettingsRoute from "./routes/SettingsRoute.svelte";
  import { showPalette, showSettings, viewState } from "./stores/view.svelte";

  // Hide-on-blur and Esc are window-level concerns (the palette is the
  // window). Components can still consume Escape inside ActionMenu / inputs
  // and stop propagation; the global handler only fires for unhandled keys.
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
    window.addEventListener("keydown", handleEscape);
    window.addEventListener("blur", handleBlur);

    const unlisteners: Array<() => void> = [];
    if (isTauri()) {
      // Tray "Open Settings" emits this event; switch route on receipt.
      void (async () => {
        const { listen } = await import("@tauri-apps/api/event");
        unlisteners.push(
          await listen<string>("nagori://navigate", (event) => {
            if (event.payload === "settings") showSettings();
            else if (event.payload === "palette") showPalette();
          }),
        );
        // Accessibility loss / paste failure: surface a toast that nudges
        // the user into Settings, where they can re-grant the permission.
        unlisteners.push(
          await listen<{ error?: string }>("nagori://paste_failed", (event) => {
            pasteFailureMessage = event.payload?.error ?? "Auto-paste failed.";
          }),
        );
      })();
    }

    return () => {
      window.removeEventListener("keydown", handleEscape);
      window.removeEventListener("blur", handleBlur);
      for (const off of unlisteners) off();
    };
  });

  let pasteFailureMessage = $state<string | null>(null);

  const dismissToast = (): void => {
    pasteFailureMessage = null;
  };

  const openSettingsFromToast = (): void => {
    showSettings();
    pasteFailureMessage = null;
  };
</script>

<main class="app-shell">
  {#if viewState.current === "palette"}
    <PaletteRoute />
  {:else}
    <SettingsRoute />
  {/if}
  {#if pasteFailureMessage}
    <div class="toast" role="status">
      <div class="toast-body">
        <strong>Auto-paste failed</strong>
        <span class="toast-detail">{pasteFailureMessage}</span>
      </div>
      <div class="toast-actions">
        <button type="button" onclick={openSettingsFromToast}>Settings</button>
        <button type="button" onclick={dismissToast}>Dismiss</button>
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

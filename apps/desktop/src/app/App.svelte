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

    let unlisten: (() => void) | undefined;
    if (isTauri()) {
      // Tray "Open Settings" emits this event; switch route on receipt.
      void (async () => {
        const { listen } = await import("@tauri-apps/api/event");
        unlisten = await listen<string>("nagori://navigate", (event) => {
          if (event.payload === "settings") showSettings();
          else if (event.payload === "palette") showPalette();
        });
      })();
    }

    return () => {
      window.removeEventListener("keydown", handleEscape);
      window.removeEventListener("blur", handleBlur);
      unlisten?.();
    };
  });
</script>

<main class="app-shell">
  {#if viewState.current === "palette"}
    <PaletteRoute />
  {:else}
    <SettingsRoute />
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
  }
</style>

<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import { hidePalette, openSettingsWindow } from './lib/commands';
  import { messages, setLocale } from './lib/i18n/index.svelte';
  import { isImeComposing } from './lib/keybindings';
  import { resolvePermissionUiState } from './lib/permissions';
  import { TAURI_EVENTS, currentWindowLabel, isTauri, subscribe } from './lib/tauri';
  import { applyAppearance } from './lib/theme';
  import type { AppSettings } from './lib/types';
  import PaletteRoute from './routes/PaletteRoute.svelte';
  import SettingsRoute from './routes/SettingsRoute.svelte';
  import { capabilitiesState } from './stores/capabilities.svelte';
  import { closeEntryContextMenu, entryContextMenuState } from './stores/entryContextMenu.svelte';
  import {
    dismissHotkeyFailure,
    hotkeyFailureState,
    startHotkeyFailureWatcher,
  } from './stores/hotkeyFailure.svelte';
  import {
    clearPasteDiagnostics,
    normalizePasteReason,
    recordPasteFailure,
  } from './stores/pasteDiagnostics.svelte';
  import { closePasteFormatPicker } from './stores/pasteFormatPicker.svelte';
  import { cancelPendingQuery, refreshCurrent } from './stores/searchQuery.svelte';
  import {
    accessibilityState,
    applySettingsSnapshot,
    refreshSettings,
    settingsState,
  } from './stores/settings.svelte';
  import { showPalette, showSettings, viewState } from './stores/view.svelte';

  // Settings runs in its own native window (`label: "settings"` in
  // `tauri.conf.json`). The palette window keeps using the in-process
  // `viewState` toggle so dev/test contexts without Tauri still work.
  const isSettingsWindow = currentWindowLabel() === 'settings';

  const t = $derived(messages());

  // Hide-on-blur and Esc are window-level concerns (the palette is the
  // window). Components can still consume Escape inside the action inspector /
  // inputs and stop propagation; the global handler only fires for unhandled keys.
  // The Settings window owns its own lifecycle (OS title-bar close, Cmd+W
  // through native decorations), so these handlers only run for the
  // palette window.
  const handleEscape = (event: KeyboardEvent): void => {
    if (event.key !== 'Escape' || event.defaultPrevented) return;
    // Escape while an IME conversion is open cancels the 変換 — it must reach
    // the input, not hide the whole palette.
    if (isImeComposing(event)) return;
    // A right-click context menu owns Escape while open: close just the menu
    // and leave the palette up. The menu normally swallows Escape itself once
    // focused (so this handler never fires); this covers the frame before its
    // focus lands, where Escape would otherwise hide the whole palette. Fully
    // consume the event — `stopImmediatePropagation` keeps the palette's own
    // window keydown listener (registered after this one) from also acting on
    // Escape and closing the palette right after we've closed the menu.
    if (entryContextMenuState.open) {
      event.preventDefault();
      event.stopImmediatePropagation();
      closeEntryContextMenu();
      return;
    }
    if (viewState.current === 'settings') {
      showPalette();
      return;
    }
    // Drop any debounced keystroke before hiding — the timer would
    // otherwise fire ~80 ms later against a closed palette and clobber
    // `searchState.results` while the next invocation is being primed.
    cancelPendingQuery();
    // A paste-format picker open at hide time must not survive into the next
    // invocation — it would re-render against a stale, now-unselected entry.
    closePasteFormatPicker();
    void hidePalette();
  };

  const handleBlur = (): void => {
    // When the user clicks away the palette is no longer useful — hide it so
    // the next hotkey press feels like a fresh invocation. Settings stays
    // visible because the user explicitly navigated there.
    if (viewState.current !== 'palette') return;
    cancelPendingQuery();
    // Don't leave a paste-format picker or context menu open behind a hidden
    // palette — either would reappear (against a stale target) on the next show.
    closePasteFormatPicker();
    closeEntryContextMenu();
    void hidePalette();
  };

  const handleFocus = (): void => {
    // The Settings window runs in a separate webview with its own store, so a
    // grant made there never reaches this palette webview's
    // `settingsState`. Re-fetch when the palette regains focus (the natural
    // moment the user returns after using the Setup tab) so the StatusBar
    // accessibility indicator clears and the success toast can fire on the
    // resulting NotGranted→Granted transition.
    void refreshSettings();
  };

  onMount(() => {
    // Hotkey registration failures are subscribed at App level — startup
    // races (the backend can emit before any window's listener has
    // attached) leak past a SettingsView-only subscription. The watcher
    // also feeds `hotkeyFailureState`, which SettingsView reads to render
    // an inline warning under the affected HotkeyInput. It runs in both
    // windows so that store stays current, but toasts only ever render in
    // the palette (see the template's `isSettingsWindow` guard) — Settings
    // has its own inline surfaces and never shows a toast (§3.4).
    const offHotkeyFailure = startHotkeyFailureWatcher();

    if (isSettingsWindow) {
      // No paste-failed subscription here: toasts are palette-only. The
      // Settings window surfaces permission state through the Setup tab
      // and the Capability table instead.
      return () => {
        offHotkeyFailure();
      };
    }

    window.addEventListener('keydown', handleEscape);
    window.addEventListener('blur', handleBlur);
    window.addEventListener('focus', handleFocus);

    // Legacy in-window navigation. The tray now opens Settings as a
    // standalone window via the `open_settings` IPC, but we keep this
    // handler so a future caller emitting the event still lands somewhere
    // reasonable (palette dev mode, non-Tauri tests).
    const offNavigate = subscribe<string>(TAURI_EVENTS.navigate, (payload) => {
      if (payload === 'settings') showSettings();
      else if (payload === 'palette') showPalette();
    });
    // Auto-paste failure. Suppress the toast when the failure is the
    // already-known "Accessibility not granted" state — the StatusBar
    // indicator covers that case, so a toast would just double up (§3.4).
    // A failure while the grant IS in place (e.g. the target app rejected
    // the synthetic paste) is genuinely unexpected and still toasts.
    const offPasteFailed = subscribe<{ error?: string; reason?: string; tool?: string }>(
      TAURI_EVENTS.pasteFailed,
      (payload) => {
        const reason = normalizePasteReason(payload?.reason);
        const message = payload?.error ?? messages().toasts.autoPasteFailedFallback;
        // Only classified auto-paste-synthesis failures carry a `reason` and
        // earn a persistent StatusBar diagnostic chip. Reason-less events are
        // copy / settings failures where the clipboard was never updated — they
        // toast (so the hidden palette's error still surfaces) but leave no
        // chip, since "copy succeeded — paste manually" would be wrong there.
        if (payload?.reason !== undefined) {
          recordPasteFailure({
            reason,
            message,
            ...(payload?.tool !== undefined ? { tool: payload.tool } : {}),
          });
        }
        if (shouldSuppressPasteToast(reason)) return;
        pasteFailureMessage = message;
      },
    );
    // Settings lives in its own webview, so a change made there reaches this
    // palette webview only through the broadcast `settingsChanged` event.
    // Without re-applying here the palette kept its startup configuration until
    // the next launch. Appearance and locale don't live in `settingsState`
    // (the theme is DOM/CSS state, the locale is the i18n module's own
    // `$state`), so they're re-applied from the payload directly. Everything
    // else the palette reads — row count, preview pane, palette hotkeys,
    // paste-format default — flows from `settingsState`, so adopting the
    // snapshot makes those `$derived` surfaces update live. `recentOrder` is
    // applied backend-side as a search runs, so the visible list keeps its old
    // order until we re-issue the current query. OS-level light/dark switches
    // in `system` mode are handled separately by `watchSystemTheme`.
    const offSettings = subscribe<AppSettings>(TAURI_EVENTS.settingsChanged, (next) => {
      applyAppearance(next.appearance);
      setLocale(next.locale);
      const previousRecentOrder = settingsState.settings?.recentOrder;
      applySettingsSnapshot(next);
      // Re-sort the visible list when the order changed. A `settings_changed`
      // event only fires on a real edit, so an unknown previous value (the
      // snapshot arrived before the first `refreshSettings` hydrated
      // `settingsState`) still means the order changed under any results
      // already on screen — re-issue the query rather than skip.
      if (previousRecentOrder !== next.recentOrder) {
        void refreshCurrent();
      }
    });

    return () => {
      window.removeEventListener('keydown', handleEscape);
      window.removeEventListener('blur', handleBlur);
      window.removeEventListener('focus', handleFocus);
      offNavigate();
      offPasteFailed();
      offSettings();
      offHotkeyFailure();
    };
  });

  // 5-state permission model shared with the SetupRoute card and the
  // StatusBar indicator. Drives both the paste-failure suppression above
  // and the success-confirmation toast below.
  const accessibilityUiState = $derived(
    resolvePermissionUiState(
      accessibilityState(),
      settingsState.settings?.onboarding,
      capabilitiesState.capabilities?.platform,
    ),
  );
  // Suppress the auto-paste failure toast only for an `accessibilityMissing`
  // failure in the not-yet-granted states the StatusBar accessibility chip
  // already explains, where a toast would just double up (§3.4).
  // `RevokedAfterGranted` is deliberately *not* suppressed: the revoke itself
  // is detected passively without a toast, but the next real paste attempt
  // that fails should surface one so the failure is tied to the user's intent
  // (S4 step 5). Every other reason — a missing `wtype`, a timeout, an
  // unrecoverable synth failure — is a genuine error the StatusBar's own
  // diagnostic chip surfaces silently, so the toast still fires to confirm the
  // copy succeeded and explain what to do next.
  const shouldSuppressPasteToast = (reason: ReturnType<typeof normalizePasteReason>): boolean => {
    if (reason !== 'accessibilityMissing') return false;
    return (
      accessibilityUiState === 'NotRequested' || accessibilityUiState === 'PromptShownNotGranted'
    );
  };

  // Brief success confirmation: when the grant flips to `Granted` from a
  // not-yet-granted state, flash a ✓ toast for 2 s so the user gets
  // immediate feedback that the Setup flow worked. Seeded from the first
  // observed state so an already-granted launch does not toast, and only
  // fires on a genuine transition (not a re-render at the same state).
  const ACCESSIBILITY_CONFIRM_MS = 2000;
  let previousAccessibilityState: ReturnType<typeof resolvePermissionUiState> | undefined;
  let accessibilityConfirmTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (isSettingsWindow) return;
    // Wait for the first real *permission* snapshot before seeding. The store
    // starts empty, so the resolver reads `NotRequested` pre-hydration; seeding
    // from that and then observing the genuine `Granted` snapshot would look
    // like a NotRequested→Granted transition and flash a spurious ✓ toast on
    // cold start. `loaded` alone is not enough: `refreshSettings` flips it true
    // even when only the permission leg failed (the settings leg succeeded), so
    // we'd seed from an empty-permission `NotRequested` and re-flash the toast
    // once the permission probe later succeeds as granted. Require the
    // permission leg to have actually landed.
    if (!settingsState.loaded || settingsState.permissionsErrorMessage !== undefined) return;
    const current = accessibilityUiState;
    const previous = previousAccessibilityState;
    previousAccessibilityState = current;
    if (previous === undefined) return; // first real observation — seed only
    if (previous === 'Granted' || current !== 'Granted') return;
    // The grant lands: any lingering accessibility-driven paste diagnostic is
    // now stale, so clear the StatusBar chip alongside the ✓ confirmation.
    clearPasteDiagnostics();
    accessibilityConfirmMessage = messages().toasts.accessibilityGrantedTitle;
    if (accessibilityConfirmTimer !== undefined) clearTimeout(accessibilityConfirmTimer);
    accessibilityConfirmTimer = setTimeout(() => {
      accessibilityConfirmMessage = null;
      accessibilityConfirmTimer = undefined;
    }, ACCESSIBILITY_CONFIRM_MS);
  });

  onDestroy(() => {
    if (accessibilityConfirmTimer !== undefined) {
      clearTimeout(accessibilityConfirmTimer);
      accessibilityConfirmTimer = undefined;
    }
  });

  let pasteFailureMessage = $state<string | null>(null);
  let accessibilityConfirmMessage = $state<string | null>(null);

  const hotkeyFailureMessage = $derived.by<string | null>(() => {
    const failure = hotkeyFailureState.failure;
    if (!failure) return null;
    return failure.error || failure.hotkey || messages().toasts.hotkeyRegisterFailedFallback;
  });

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

  const openSettingsFromHotkeyToast = (): void => {
    if (isTauri()) {
      void openSettingsWindow();
    } else {
      showSettings();
    }
    dismissHotkeyFailure();
  };
</script>

<main class="app-shell" class:settings-window={isSettingsWindow}>
  {#if isSettingsWindow}
    <SettingsRoute />
  {:else if viewState.current === 'palette'}
    <PaletteRoute />
  {:else}
    <SettingsRoute />
  {/if}
  {#if !isSettingsWindow && (pasteFailureMessage || hotkeyFailureMessage || accessibilityConfirmMessage)}
    <div class="toast-stack">
      {#if hotkeyFailureMessage}
        <div class="toast toast-hotkey" role="status">
          <div class="toast-body">
            <strong>{t.toasts.hotkeyRegisterFailedTitle}</strong>
            <span class="toast-detail">{hotkeyFailureMessage}</span>
          </div>
          <div class="toast-actions">
            <button type="button" onclick={openSettingsFromHotkeyToast}>
              {t.toasts.openSettings}
            </button>
            <button type="button" onclick={dismissHotkeyFailure}>{t.toasts.dismiss}</button>
          </div>
        </div>
      {/if}
      {#if accessibilityConfirmMessage}
        <div class="toast toast-success" role="status">
          <div class="toast-body">
            <strong>{accessibilityConfirmMessage}</strong>
          </div>
        </div>
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
  .toast-stack {
    position: absolute;
    bottom: 0.75rem;
    right: 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    max-width: 22rem;
  }
  .toast {
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
  .toast-hotkey {
    background: rgba(40, 32, 16, 0.92);
    color: #ffe9c8;
    border-color: rgba(255, 180, 90, 0.5);
  }
  .toast-hotkey .toast-detail {
    color: rgba(255, 233, 200, 0.85);
  }
  .toast-success {
    background: rgba(16, 40, 24, 0.92);
    color: #c8f0d4;
    border-color: rgba(120, 200, 140, 0.5);
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

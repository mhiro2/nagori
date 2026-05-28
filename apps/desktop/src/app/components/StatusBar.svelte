<script lang="ts">
  import { openSettingsWindow, setCaptureEnabled } from '../lib/commands';
  import { describeError } from '../lib/errors';
  import { messages } from '../lib/i18n/index.svelte';
  import { formatAccelerator } from '../lib/keybindings';
  import { resolvePermissionUiState } from '../lib/permissions';
  import { isTauri } from '../lib/tauri';
  import { capabilitiesState } from '../stores/capabilities.svelte';
  import { accessibilityState, captureEnabled, settingsState } from '../stores/settings.svelte';
  import { showSettings } from '../stores/view.svelte';

  type Props = {
    entryCount: number;
    elapsedMs: number | undefined;
    loading: boolean;
    errorMessage: string | undefined;
    selectedCount?: number;
  };

  const { entryCount, elapsedMs, loading, errorMessage, selectedCount = 0 }: Props = $props();
  const t = $derived(messages());

  // Hint glyphs follow the host platform — `⌘K` / `⌘,` on macOS, `Ctrl+K`
  // / `Ctrl+,` on Windows/Linux — so the row matches the modifier the
  // user actually presses. `formatAccelerator` does the per-OS render
  // (mac contiguous glyphs vs the `Ctrl+...` join the rest of the OS
  // chrome uses) and folds CmdOrCtrl to the correct primary key.
  const platform = $derived(capabilitiesState.capabilities?.platform);
  const hintActions = $derived(formatAccelerator('CmdOrCtrl+K', platform));
  const hintSettings = $derived(formatAccelerator('CmdOrCtrl+,', platform));

  // Outside Tauri there's no settings store to read from (refreshSettings
  // only flips `loaded`), so `localCapture` lets the demo chip still reflect
  // clicks. Under Tauri it stays `undefined` and the store remains the source
  // of truth.
  let localCapture = $state<boolean | undefined>(undefined);
  const capture = $derived(localCapture ?? captureEnabled());

  // Lightweight accessibility indicator. Replaces the legacy OnboardingBanner
  // (a ~60-line card) with a one-row hint: when the OS-level grant required
  // for auto-paste is missing the palette surfaces the warning + Setup CTA
  // here, and hides the row entirely once the grant lands. The 5-state
  // resolver lives in `lib/permissions.ts` so this row stays in lockstep
  // with the SetupRoute card's view of the same status (e.g. it correctly
  // suppresses the warning on `Unavailable` platforms where there is no
  // grant to chase).
  const accessibilityUiState = $derived(
    resolvePermissionUiState(
      accessibilityState(),
      settingsState.settings?.onboarding,
      capabilitiesState.capabilities?.platform,
    ),
  );
  // Show the indicator while we genuinely need a grant — `Unavailable`
  // platforms (Windows, Wayland without `wtype`, etc.) have nothing the
  // user can act on, so the row would just nag. Gate on the capability
  // snapshot having loaded so we don't flash the warning on every palette
  // open before `get_capabilities` resolves (the status defaults to
  // `NotRequested` until then).
  const showAccessibilityWarning = $derived(
    capabilitiesState.capabilities !== undefined &&
      (accessibilityUiState === 'NotRequested' ||
        accessibilityUiState === 'PromptShownNotGranted' ||
        accessibilityUiState === 'RevokedAfterGranted'),
  );

  const openSetup = (): void => {
    // Standalone Settings window under Tauri (own decorations, no
    // always-on-top). The `'setup'` route hint asks SettingsView to land
    // on the Setup tab regardless of the first-launch heuristic — which
    // would otherwise drop a previously-granted-then-revoked user on
    // General.
    if (isTauri()) void openSettingsWindow('setup');
    else showSettings();
  };

  // The capture chip is a toggle, matching the tray's "Pause/Resume capture"
  // item. We `await` the IPC rather than optimistically flipping the store:
  // capture toggling is a low-frequency action, so authoritative state from
  // the returned `AppSettings` is simpler than a rollback path. `pending`
  // debounces double-clicks so a second press can't race the in-flight call.
  let pending = $state(false);
  const toggleCapture = async (): Promise<void> => {
    if (pending) return;
    const next = !capture;
    if (!isTauri()) {
      localCapture = next;
      return;
    }
    pending = true;
    try {
      settingsState.settings = await setCaptureEnabled(next);
    } catch (err) {
      // Surface the failure on the existing global error channel (it blanks
      // the count until the next refresh) rather than letting the rejection
      // go unhandled — the chip would otherwise silently snap back.
      settingsState.errorMessage = describeError(err);
    } finally {
      pending = false;
    }
  };
</script>

<footer class="status">
  <span class="left">
    <!-- Only the volatile summary text truncates under pressure; the warning
         chip stays whole (its own `flex: 0 0 auto`) so the focus ring and
         border are never clipped. -->
    <span class="summary">
      {#if errorMessage}
        <span class="error">{errorMessage}</span>
      {:else if loading}
        <span>{t.palette.searching}</span>
      {:else}
        <span>{t.status.entryCount(entryCount)}</span>
        {#if elapsedMs !== undefined}
          <span class="dot">·</span>
          <span>{t.palette.elapsed(elapsedMs)}</span>
        {/if}
        {#if selectedCount > 0}
          <span class="dot">·</span>
          <span class="multi">{t.status.selectedCount(selectedCount)}</span>
        {/if}
      {/if}
    </span>
    {#if showAccessibilityWarning}
      <button
        type="button"
        class="chip warning-chip"
        title={t.status.autoPasteOff}
        aria-label={t.status.autoPasteOffSetupAria}
        onclick={openSetup}
      >
        {t.status.autoPasteOffShort}
      </button>
    {/if}
  </span>
  <span class="right">
    <button
      type="button"
      class="chip capture-chip"
      class:on={capture}
      class:off={!capture}
      aria-pressed={capture}
      disabled={pending}
      onclick={() => void toggleCapture()}
    >
      <span class="dot-icon" aria-hidden="true"></span>
      {capture ? t.status.captureOn : t.status.capturePaused}
    </button>
    <!-- Keyboard hints are the lowest-priority row content: drop them while
         the warning chip is present so the bar never wraps on a narrow
         palette (priority: warning > capture > summary > hints). -->
    {#if !showAccessibilityWarning}
      <span class="hints">
        <kbd>↑↓</kbd>{t.palette.hints.navigate}
        <kbd>Enter</kbd>{t.palette.hints.paste}
        <kbd>{hintActions}</kbd>{t.palette.hints.actions}
        <kbd>{hintSettings}</kbd>{t.palette.hints.settings}
      </span>
    {/if}
  </span>
</footer>

<style>
  .status {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.4rem 1rem;
    border-top: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    background: var(--bg-elevated, rgba(255, 255, 255, 0.02));
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .left,
  .right {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  /* The left column gives way under pressure (so its summary truncates),
     while the right column holds its intrinsic width and never wraps. */
  .left {
    flex: 1 1 auto;
    min-width: 0;
  }
  .right {
    flex: 0 0 auto;
  }
  /* Summary is the only shrinkable piece: it ellipsis-truncates when the
     warning chip needs room, instead of pushing the chips onto a new line.
     It must stay block-level (not flex) for `text-overflow: ellipsis` to
     apply to its inline run of count/dot/elapsed spans. */
  .summary {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .error {
    color: var(--danger, #f87171);
  }
  /* Shared pill for the two interactive chips (capture toggle + accessibility
     warning), so they read as the same affordance. */
  .chip {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.05rem 0.45rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    border-radius: 999px;
    appearance: none;
    background: transparent;
    color: inherit;
    font: inherit;
    white-space: nowrap;
    cursor: pointer;
  }
  .chip:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 2px;
  }
  .chip:disabled {
    cursor: progress;
    opacity: 0.6;
  }
  .capture-chip.on {
    border-color: rgba(120, 200, 140, 0.4);
    color: var(--ok, #86d29a);
  }
  .capture-chip.off {
    color: var(--muted, rgba(255, 255, 255, 0.4));
  }
  .capture-chip:not(:disabled):hover {
    background: rgba(255, 255, 255, 0.06);
  }
  .warning-chip {
    color: var(--warning, #f59e0b);
    border-color: currentColor;
  }
  .warning-chip:hover {
    background: rgba(245, 158, 11, 0.12);
  }
  .warning-chip:focus-visible {
    outline-color: var(--warning, #f59e0b);
  }
  .dot-icon {
    width: 0.4rem;
    height: 0.4rem;
    border-radius: 50%;
    background: currentColor;
  }
  .dot {
    opacity: 0.5;
    /* Spacing between summary segments — the flex `gap` no longer applies now
       that `.summary` is block-level for ellipsis. */
    margin: 0 0.4rem;
  }
  .multi {
    color: var(--accent, #6c8dff);
    font-weight: 600;
  }
  .hints {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-left: 0.25rem;
  }
  kbd {
    padding: 0.05rem 0.35rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 4px;
    font-family: inherit;
    font-size: 0.7rem;
  }
</style>

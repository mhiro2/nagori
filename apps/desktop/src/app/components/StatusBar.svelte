<script lang="ts">
  import { openSettingsWindow } from '../lib/commands';
  import { messages } from '../lib/i18n/index.svelte';
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

  const capture = $derived(captureEnabled());

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
</script>

<footer class="status">
  <span class="left">
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
    {#if showAccessibilityWarning}
      <span class="dot">·</span>
      <span class="accessibility-warning">
        <span>{t.status.autoPasteOff}</span>
        <button type="button" class="setup-cta" onclick={openSetup}>
          {t.status.openSetup}
        </button>
      </span>
    {/if}
  </span>
  <span class="right">
    <span class="badge" class:on={capture} class:off={!capture}>
      <span class="dot-icon" aria-hidden="true"></span>
      {capture ? t.status.captureOn : t.status.capturePaused}
    </span>
    <span class="hints">
      <kbd>↑↓</kbd>{t.palette.hints.navigate}
      <kbd>Enter</kbd>{t.palette.hints.paste}
      <kbd>⌘K</kbd>{t.palette.hints.actions}
      <kbd>⌘,</kbd>{t.palette.hints.settings}
    </span>
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
  .error {
    color: var(--danger, #f87171);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.05rem 0.45rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    border-radius: 999px;
  }
  .badge.on {
    border-color: rgba(120, 200, 140, 0.4);
    color: var(--ok, #86d29a);
  }
  .badge.off {
    color: var(--muted, rgba(255, 255, 255, 0.4));
  }
  .dot-icon {
    width: 0.4rem;
    height: 0.4rem;
    border-radius: 50%;
    background: currentColor;
  }
  .dot {
    opacity: 0.5;
  }
  .multi {
    color: var(--accent, #6c8dff);
    font-weight: 600;
  }
  .accessibility-warning {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--warning, #f59e0b);
  }
  .setup-cta {
    appearance: none;
    background: transparent;
    border: 1px solid currentColor;
    color: inherit;
    border-radius: 999px;
    padding: 0.025rem 0.45rem;
    font: inherit;
    font-size: 0.7rem;
    cursor: pointer;
  }
  .setup-cta:hover,
  .setup-cta:focus-visible {
    background: rgba(245, 158, 11, 0.12);
  }
  .setup-cta:focus-visible {
    outline: 2px solid var(--warning, #f59e0b);
    outline-offset: 2px;
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

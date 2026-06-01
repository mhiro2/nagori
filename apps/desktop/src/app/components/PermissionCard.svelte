<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import accessibilityScreenshotDark from '../../assets/onboarding/accessibility-mac-dark.svg';
  import accessibilityScreenshotLight from '../../assets/onboarding/accessibility-mac-light.svg';
  import { describeError } from '../lib/errors';
  import { messages } from '../lib/i18n/index.svelte';
  import {
    refreshPermissionsOnce,
    requestAccessibility,
    resolvePermissionUiState,
    subscribeToPolling,
    type PermissionUiState,
  } from '../lib/permissions';
  import type { PermissionKind } from '../lib/types';
  import { capabilitiesState } from '../stores/capabilities.svelte';
  import { accessibilityState, settingsState } from '../stores/settings.svelte';

  // Surface chosen by the Setup tab. We only ship the Accessibility flow for
  // now but the prop keeps the door open for Input Monitoring /
  // Notifications cards to reuse the same shell.
  type Props = { kind: PermissionKind };
  const { kind }: Props = $props();

  const t = $derived(messages());

  const platform = $derived(capabilitiesState.capabilities?.platform);
  const status = $derived(kind === 'accessibility' ? accessibilityState() : undefined);
  const onboarding = $derived(settingsState.settings?.onboarding);
  const uiState: PermissionUiState = $derived(
    resolvePermissionUiState(status, onboarding, platform),
  );

  let requesting = $state(false);
  // `timedOut` flips on a poller `'timeout'` event and clears on the next
  // recheck attempt or fresh subscription. The inline error stays sticky
  // until the user acts so a backgrounded webview that misses the
  // notification window does not strand them.
  let timedOut = $state(false);
  let requestErrorMessage: string | undefined = $state(undefined);

  // Drop the inline error as soon as the card lands in a terminal state —
  // a successful grant after a timeout should not nag with stale copy, and
  // an Unavailable card (Linux without `wtype`, Windows synthetic denied)
  // has no actionable retry so the timeout banner would be pure noise.
  $effect(() => {
    if (uiState === 'Granted' || uiState === 'Unavailable') {
      timedOut = false;
      requestErrorMessage = undefined;
    }
  });

  let unsubscribe: (() => void) | null = null;

  onMount(() => {
    unsubscribe = subscribeToPolling({
      onEvent: (event) => {
        // Skip the sticky timeout flag when the card has already settled into
        // a terminal state — a Granted/Unavailable card has no actionable
        // follow-up so the "we gave up waiting" copy would just nag.
        if (event === 'timeout' && uiState !== 'Granted' && uiState !== 'Unavailable') {
          timedOut = true;
        }
      },
    });
  });

  onDestroy(() => {
    unsubscribe?.();
    unsubscribe = null;
  });

  const onGrantClick = async (): Promise<void> => {
    if (requesting) return;
    requesting = true;
    timedOut = false;
    requestErrorMessage = undefined;
    try {
      await requestAccessibility(true);
      await refreshPermissionsOnce();
    } catch (err) {
      requestErrorMessage = describeError(err);
    } finally {
      requesting = false;
    }
  };

  const onRecheckClick = async (): Promise<void> => {
    timedOut = false;
    requestErrorMessage = undefined;
    await refreshPermissionsOnce();
  };

  const unavailableMessageKey = $derived.by((): keyof typeof t.setup.accessibility.messages => {
    if (platform === 'linuxWayland') return 'UnavailableLinux';
    if (platform === 'windows') return 'UnavailableWindows';
    return 'UnavailableMacosFallback';
  });

  const stateMessage = $derived(
    uiState === 'Unavailable'
      ? t.setup.accessibility.messages[unavailableMessageKey]
      : t.setup.accessibility.messages[uiState],
  );

  // CTA copy. The first attempt says "Grant Accessibility…" (TCC dialog
  // will appear); after the prompt has been fired once macOS suppresses
  // the dialog and the button morphs into a deep link to System Settings.
  const grantLabel = $derived.by(() => {
    if (uiState === 'NotRequested') return t.setup.accessibility.grantButton;
    return t.setup.accessibility.grantButtonRetry;
  });

  // Grant button is hidden in terminal states (granted, unavailable).
  const showGrantButton = $derived(uiState !== 'Granted' && uiState !== 'Unavailable');

  // Re-check pairs with the timeout error and the Granted state's omitted
  // CTA — it gives the user a manual nudge when they believe they have
  // already toggled the switch but the poller has been throttled.
  const showRecheckButton = $derived(uiState !== 'Granted' && uiState !== 'Unavailable');

  // `description` is per-platform: the macOS copy walks the user through
  // the TCC dialog, but there is no such pane on Windows (auto-paste needs
  // no permission) or Linux (it relies on the `wtype` helper), so those
  // hosts must not be told to "open the macOS dialog".
  const description = $derived(
    platform === 'linuxWayland'
      ? t.setup.accessibility.descriptionLinux
      : platform === 'windows'
        ? t.setup.accessibility.descriptionWindows
        : t.setup.accessibility.description,
  );

  const showScreenshot = $derived(platform === 'macos' && uiState !== 'Granted');
</script>

<article class="permission-card" data-state={uiState} data-kind={kind}>
  <header class="head">
    <div class="title-row">
      <h2>{t.setup.accessibility.title}</h2>
      <span class="required-pill">{t.setup.accessibility.required}</span>
    </div>
    <span class="state-pill" data-state={uiState}>
      <span class="state-label">{t.setup.accessibility.statusLabel}:</span>
      {t.setup.accessibility.states[uiState]}
    </span>
  </header>

  <p class="description">{description}</p>

  {#if showScreenshot}
    <picture class="screenshot">
      <source srcset={accessibilityScreenshotDark} media="(prefers-color-scheme: dark)" />
      <img
        src={accessibilityScreenshotLight}
        alt={t.setup.accessibility.screenshotAlt}
        loading="lazy"
        decoding="async"
      />
    </picture>
  {/if}

  <p class="state-message">{stateMessage}</p>

  {#if timedOut}
    <p class="inline-error" role="alert">{t.setup.accessibility.timeoutError}</p>
  {/if}
  {#if requestErrorMessage}
    <p class="inline-error" role="alert">
      {t.setup.accessibility.requestError}
      <span class="error-detail">{requestErrorMessage}</span>
    </p>
  {/if}

  {#if showGrantButton || showRecheckButton}
    <div class="actions">
      {#if showGrantButton}
        <button
          type="button"
          class="primary"
          disabled={requesting}
          onclick={() => void onGrantClick()}
        >
          {requesting ? t.setup.accessibility.requesting : grantLabel}
        </button>
      {/if}
      {#if showRecheckButton}
        <button type="button" class="secondary" onclick={() => void onRecheckClick()}>
          {t.setup.accessibility.recheckButton}
        </button>
      {/if}
    </div>
  {/if}
</article>

<style>
  .permission-card {
    display: grid;
    gap: 0.75rem;
    padding: 1rem 1.1rem;
    border: 1px solid var(--card-border, rgba(255, 255, 255, 0.08));
    border-radius: 10px;
    background: var(--card-bg, rgba(255, 255, 255, 0.03));
    color: var(--fg, #f5f5f5);
  }
  .permission-card[data-state='Granted'] {
    border-color: var(--success-border, rgba(48, 209, 88, 0.45));
    background: var(--success-bg, rgba(48, 209, 88, 0.07));
  }
  .permission-card[data-state='Unavailable'] {
    border-color: var(--muted-border, rgba(255, 255, 255, 0.08));
    background: var(--muted-bg, rgba(255, 255, 255, 0.02));
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  h2 {
    margin: 0;
    font-size: 1rem;
    font-weight: 600;
  }
  .required-pill {
    padding: 0.05rem 0.4rem;
    border-radius: 9999px;
    background: var(--warning-bg, rgba(245, 158, 11, 0.18));
    color: var(--warning, #f59e0b);
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .state-pill {
    padding: 0.15rem 0.55rem;
    border-radius: 9999px;
    background: var(--muted-bg, rgba(255, 255, 255, 0.06));
    color: var(--muted, rgba(255, 255, 255, 0.7));
    font-size: 0.75rem;
    font-weight: 500;
  }
  .state-pill .state-label {
    margin-right: 0.25rem;
    opacity: 0.8;
  }
  .permission-card[data-state='Granted'] .state-pill {
    background: var(--success-pill-bg, rgba(48, 209, 88, 0.18));
    color: var(--success, #34c759);
  }
  .permission-card[data-state='PromptShownNotGranted'] .state-pill,
  .permission-card[data-state='RevokedAfterGranted'] .state-pill {
    background: var(--warning-bg, rgba(245, 158, 11, 0.18));
    color: var(--warning, #f59e0b);
  }
  .description {
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.75));
    font-size: 0.85rem;
    line-height: 1.4;
  }
  .screenshot {
    display: block;
    overflow: hidden;
    border-radius: 8px;
    background: var(--screenshot-bg, rgba(255, 255, 255, 0.04));
  }
  .screenshot img {
    display: block;
    width: 100%;
    height: auto;
  }
  .state-message {
    margin: 0;
    font-size: 0.8125rem;
    line-height: 1.45;
    color: var(--fg, #f5f5f5);
  }
  .inline-error {
    margin: 0;
    padding: 0.5rem 0.65rem;
    border: 1px solid var(--warning-border, rgba(245, 158, 11, 0.4));
    border-radius: 8px;
    background: var(--warning-bg, rgba(245, 158, 11, 0.08));
    color: var(--warning, #f59e0b);
    font-size: 0.8125rem;
    line-height: 1.4;
  }
  .error-detail {
    display: block;
    margin-top: 0.2rem;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.75rem;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .primary {
    padding: 0.4rem 0.95rem;
    border: 1px solid transparent;
    border-radius: 6px;
    background: var(--accent, #6c8dff);
    color: var(--bg, #14161a);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .primary:disabled {
    cursor: progress;
    opacity: 0.7;
  }
  .secondary {
    padding: 0.4rem 0.95rem;
    border: 1px solid var(--card-border, rgba(255, 255, 255, 0.16));
    border-radius: 6px;
    background: transparent;
    color: var(--fg, #f5f5f5);
    font: inherit;
    cursor: pointer;
  }
  .secondary:hover {
    background: var(--card-hover, rgba(255, 255, 255, 0.06));
  }
</style>

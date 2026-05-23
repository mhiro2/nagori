<script lang="ts">
  import { openAccessibilitySettings } from '../lib/commands';
  import { messages } from '../lib/i18n/index.svelte';
  import { isTauri } from '../lib/tauri';
  import { capabilitiesState } from '../stores/capabilities.svelte';
  import {
    accessibilityGranted,
    accessibilityState,
    settingsState,
  } from '../stores/settings.svelte';

  // Open the macOS Privacy → Accessibility pane via the backend `open(1)`
  // shim. Webview navigation can't follow `x-apple.systempreferences:` URLs.
  const openSettings = async (): Promise<void> => {
    if (!isTauri()) return;
    try {
      await openAccessibilitySettings();
    } catch {
      // Best-effort.
    }
  };

  let dismissed = $state(false);
  const t = $derived(messages());
  // Linux Wayland gates auto-paste on the `wtype` helper instead of an
  // OS-level Accessibility toggle. Branch the copy and hide the
  // "Open System Settings" button — there is no equivalent pane.
  const platform = $derived(capabilitiesState.capabilities?.platform);
  const isLinux = $derived(platform === 'linuxWayland');
  // `open_accessibility_settings` only lands the user on a real pane on
  // macOS; the Windows/Linux backends return `Unsupported`. Gate the
  // CTA on `'macos'` (rather than `!isLinux`) so the button never
  // points at a no-op.
  const showOpenSettings = $derived(platform === 'macos');
  const description = $derived(isLinux ? t.onboarding.descriptionLinux : t.onboarding.description);
  const requiredLabel = $derived(
    isLinux ? t.onboarding.accessibilityRequiredLinux : t.onboarding.accessibilityRequired,
  );
  // Prefer the backend's permission `message` (carries the live wtype
  // probe error / install hint); fall back to the localised default so
  // the banner still reads sensibly when the field is empty.
  const hint = $derived(
    accessibilityState()?.message ??
      (isLinux ? t.onboarding.accessibilityHintLinux : t.onboarding.accessibilityHint),
  );
  const autoPasteDisabled = $derived(
    isLinux ? t.onboarding.autoPasteDisabledLinux : t.onboarding.autoPasteDisabled,
  );
  // Show the banner once permissions have been loaded and accessibility
  // is not granted. Re-evaluates if `refreshSettings` repopulates the store.
  const visible = $derived(
    settingsState.loaded &&
      !dismissed &&
      accessibilityState() !== undefined &&
      !accessibilityGranted(),
  );
</script>

{#if visible}
  <aside class="onboarding" role="status" aria-live="polite">
    <div class="head">
      <strong>{t.onboarding.title}</strong>
      <button type="button" class="close" onclick={() => (dismissed = true)}> × </button>
    </div>
    <p class="desc">{description}</p>
    <ul class="items">
      <li>
        <span class="label">{requiredLabel}</span>
        <p class="hint">{hint}</p>
        <p class="hint warn">{autoPasteDisabled}</p>
      </li>
      {#if !isLinux}
        <li class="muted">
          <p class="hint">{t.onboarding.notificationsHint}</p>
        </li>
      {/if}
    </ul>
    <div class="actions">
      {#if showOpenSettings}
        <button type="button" class="primary" onclick={() => void openSettings()}>
          {t.onboarding.openSettings}
        </button>
      {/if}
      <button type="button" class="link" onclick={() => (dismissed = true)}>
        {t.onboarding.dismiss}
      </button>
    </div>
  </aside>
{/if}

<style>
  .onboarding {
    margin: 0.5rem 0.75rem 0;
    padding: 0.75rem 1rem;
    border: 1px solid var(--warning-border, rgba(245, 158, 11, 0.4));
    border-radius: 8px;
    background: var(--warning-bg, rgba(245, 158, 11, 0.08));
    color: var(--fg, #f5f5f5);
    font-size: 0.8125rem;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.25rem;
  }
  .close {
    width: 1.5rem;
    height: 1.5rem;
    border: none;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 1rem;
    cursor: pointer;
  }
  .desc {
    margin: 0 0 0.5rem;
    color: var(--muted, rgba(255, 255, 255, 0.7));
  }
  .items {
    list-style: none;
    margin: 0 0 0.5rem;
    padding: 0;
    display: grid;
    gap: 0.4rem;
  }
  .label {
    font-weight: 600;
  }
  .hint {
    margin: 0.1rem 0 0;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.75rem;
  }
  .hint.warn {
    color: var(--warning, #f59e0b);
  }
  .muted {
    color: var(--muted, rgba(255, 255, 255, 0.5));
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .primary {
    padding: 0.35rem 0.85rem;
    border: 1px solid transparent;
    border-radius: 6px;
    background: var(--accent, #6c8dff);
    color: var(--bg, #14161a);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .link {
    border: none;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font: inherit;
    cursor: pointer;
    text-decoration: underline;
  }
</style>

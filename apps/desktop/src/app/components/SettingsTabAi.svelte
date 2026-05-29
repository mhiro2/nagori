<script lang="ts">
  import { getAiAvailability } from '../lib/commands';
  import type { Messages } from '../lib/i18n/locales/en';
  import { isTauri } from '../lib/tauri';
  import type { AiAvailability, AiProviderKind, AppSettings } from '../lib/types';

  type Props = {
    settings: AppSettings;
    t: Messages;
    scheduleSave: (delay: number) => void;
  };

  let { settings = $bindable(), t, scheduleSave }: Props = $props();

  let availability = $state<AiAvailability | undefined>(undefined);

  // Re-probe availability whenever AI is enabled (or the tab re-renders with
  // it on), so the status line reflects the live Apple Intelligence state.
  $effect(() => {
    if (!isTauri() || !settings.ai.enabled) {
      availability = undefined;
      return;
    }
    void (async () => {
      try {
        availability = await getAiAvailability();
      } catch {
        availability = undefined;
      }
    })();
  });

  const statusLabel = $derived.by(() => {
    if (!settings.ai.enabled) return t.settings.ai.statusDisabled;
    switch (availability?.overallStatus) {
      case 'available':
        return t.settings.ai.statusAvailable;
      case 'unavailable':
        return t.settings.ai.statusUnavailable;
      default:
        return t.settings.ai.statusDisabled;
    }
  });

  const onProviderChange = (event: Event): void => {
    const value = (event.currentTarget as HTMLSelectElement).value as AiProviderKind;
    settings.ai.provider = value;
    scheduleSave(0);
  };
</script>

<fieldset>
  <legend>{t.settings.ai.legend}</legend>
  <label>
    <input type="checkbox" bind:checked={settings.ai.enabled} onchange={() => scheduleSave(0)} />
    {t.settings.ai.enabled}
  </label>
  <p class="help">{t.settings.ai.enabledHelp}</p>

  <label class="stack">
    <span>{t.settings.ai.provider}</span>
    <select value={settings.ai.provider} onchange={onProviderChange}>
      <option value="disabled">{t.settings.ai.providerDisabled}</option>
      <option value="appleNative">{t.settings.ai.providerApple}</option>
    </select>
  </label>

  <label>
    <input
      type="checkbox"
      bind:checked={settings.ai.allowStreaming}
      onchange={() => scheduleSave(0)}
    />
    {t.settings.ai.allowStreaming}
  </label>
  <p class="help">{t.settings.ai.allowStreamingHelp}</p>

  <p class="status">{t.settings.ai.status}: {statusLabel}</p>
</fieldset>

<fieldset>
  <legend>{t.settings.ai.semanticIndex}</legend>
  <label>
    <input
      type="checkbox"
      bind:checked={settings.ai.semanticIndexEnabled}
      onchange={() => scheduleSave(0)}
    />
    {t.settings.ai.semanticIndex}
  </label>
  <p class="help">{t.settings.ai.semanticIndexHelp}</p>
</fieldset>

<style>
  .help {
    margin: 0.25rem 0 0.75rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.8125rem;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin: 0.5rem 0;
  }
  .status {
    margin: 0.5rem 0 0;
    font-size: 0.8125rem;
    color: var(--muted, rgba(255, 255, 255, 0.6));
  }
</style>

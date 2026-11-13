<script lang="ts">
  import { getAiAvailability, getSemanticIndexStatus, rebuildSemanticIndex } from '../lib/commands';
  import type { Messages } from '../lib/i18n/locales/en';
  import { isTauri } from '../lib/tauri';
  import type {
    AiAvailability,
    AiProviderKind,
    AppSettings,
    SemanticIndexStatus,
  } from '../lib/types';
  import { imageTextRecognitionSupported } from '../stores/capabilities.svelte';

  type Props = {
    settings: AppSettings;
    t: Messages;
    scheduleSave: (delay: number) => void;
    // Bumped by the parent after each successful settings save. The probe
    // reads the stored settings, so a toggle answered before its save lands
    // would describe the previous state; re-probing on each save catches up.
    savedRevision?: number;
  };

  let { settings = $bindable(), t, scheduleSave, savedRevision = 0 }: Props = $props();

  let availability = $state<AiAvailability | undefined>(undefined);
  let availabilityFailed = $state(false);
  let semanticStatus = $state<SemanticIndexStatus | undefined>(undefined);

  // Re-probe availability whenever AI is turned on, the provider changes or a
  // save lands, so the status line reflects the live Apple Intelligence state.
  // A probe that a newer one has replaced drops its answer.
  let probeSeq = 0;
  $effect(() => {
    const enabled = settings.ai.enabled;
    void settings.ai.provider;
    void savedRevision;
    probeSeq += 1;
    const seq = probeSeq;
    if (!isTauri() || !enabled) {
      availability = undefined;
      availabilityFailed = false;
      return;
    }
    void (async () => {
      try {
        const next = await getAiAvailability();
        if (seq !== probeSeq) return;
        availability = next;
        availabilityFailed = false;
      } catch {
        if (seq !== probeSeq) return;
        availability = undefined;
        availabilityFailed = true;
      }
    })();
  });

  const refreshSemanticStatus = async (): Promise<void> => {
    try {
      semanticStatus = await getSemanticIndexStatus();
    } catch {
      semanticStatus = undefined;
    }
  };

  // Poll the semantic index status while the index is enabled so the progress
  // line tracks the background worker as it embeds the backlog.
  $effect(() => {
    if (!isTauri() || !settings.ai.semanticIndexEnabled) {
      semanticStatus = undefined;
      return;
    }
    void refreshSemanticStatus();
    const timer = setInterval(() => void refreshSemanticStatus(), 2000);
    return () => clearInterval(timer);
  });

  // The AI switch and the provider are both user choices; the status line
  // only reports what the probe found for them. A probe that has not answered,
  // failed, or still describes the previous provider or switch position is
  // not reported as "off".
  const statusLabel = $derived.by(() => {
    if (!settings.ai.enabled) return t.settings.ai.statusDisabled;
    if (settings.ai.provider === 'disabled') return t.settings.ai.statusNoProvider;
    if (availabilityFailed) return t.settings.ai.statusUnknown;
    if (!availability || availability.provider !== settings.ai.provider) {
      return t.settings.ai.statusChecking;
    }
    switch (availability.overallStatus) {
      case 'available':
        return t.settings.ai.statusAvailable;
      case 'unavailable':
        return t.settings.ai.statusUnavailable;
      default:
        return t.settings.ai.statusChecking;
    }
  });

  const semanticStateLabel = $derived.by(() => {
    switch (semanticStatus?.state) {
      case 'ready':
        return t.settings.ai.semanticIndexStateReady;
      case 'indexing':
        return t.settings.ai.semanticIndexStateIndexing;
      case 'paused':
        return t.settings.ai.semanticIndexStatePaused;
      case 'unavailable':
        return t.settings.ai.semanticIndexStateUnavailable;
      case 'unsupported':
        return t.settings.ai.semanticIndexStateUnsupported;
      default:
        return t.settings.ai.semanticIndexStateDisabled;
    }
  });

  // Detail appended to the state label. While the worker is still embedding the
  // backlog, show how far along it is (percent + remaining) so "Indexing…"
  // carries progress, not just a running count; once it settles — or there's
  // nothing to index — fall back to the plain indexed/total.
  const semanticDetail = $derived.by(() => {
    if (!semanticStatus || semanticStatus.total === 0) return undefined;
    const { state, indexed, total, pending } = semanticStatus;
    if (state === 'indexing') {
      const percent = Math.floor((indexed / total) * 100);
      return t.settings.ai.semanticIndexProgress({ percent, indexed, total, pending });
    }
    return `(${indexed} / ${total})`;
  });

  const onProviderChange = (event: Event): void => {
    const value = (event.currentTarget as HTMLSelectElement).value as AiProviderKind;
    settings.ai.provider = value;
    scheduleSave(0);
  };

  const onRebuild = (): void => {
    if (!isTauri()) return;
    void rebuildSemanticIndex();
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

  <p class="status" data-testid="ai-status" aria-live="polite">
    {t.settings.ai.status}: {statusLabel}
  </p>
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

  {#if settings.ai.semanticIndexEnabled}
    <label>
      <input
        type="checkbox"
        bind:checked={settings.ai.semanticIndexAcPowerOnly}
        onchange={() => scheduleSave(0)}
      />
      {t.settings.ai.semanticIndexAcPowerOnly}
    </label>
    <p class="help">{t.settings.ai.semanticIndexAcPowerOnlyHelp}</p>

    <div class="index-row">
      <button type="button" onclick={onRebuild}>{t.settings.ai.semanticIndexRebuild}</button>
      {#if semanticStatus}
        <span class="status">
          {t.settings.ai.semanticIndexStatus}: {semanticStateLabel}
          {#if semanticDetail}
            {semanticDetail}
          {/if}
        </span>
      {/if}
    </div>
  {/if}
</fieldset>

{#if imageTextRecognitionSupported()}
  <fieldset>
    <legend>{t.settings.ai.imageTextIndex}</legend>
    <label>
      <input
        type="checkbox"
        data-testid="image-text-index-toggle"
        bind:checked={settings.ai.imageTextIndexEnabled}
        onchange={() => scheduleSave(0)}
      />
      {t.settings.ai.imageTextIndex}
    </label>
    <p class="help">{t.settings.ai.imageTextIndexHelp}</p>
  </fieldset>
{/if}

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
  .index-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin: 0.5rem 0 0;
  }
  .index-row .status {
    margin: 0;
  }
</style>

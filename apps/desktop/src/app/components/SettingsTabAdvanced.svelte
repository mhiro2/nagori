<script lang="ts">
  import type { Messages } from '../lib/i18n/locales/en';
  import type { AppSettings, Capability, PlatformCapabilities } from '../lib/types';

  type CapabilityRowKey = keyof Messages['settings']['capabilities']['rows'];

  type CapabilityRow = {
    key: CapabilityRowKey;
    label: string;
    capability: Capability;
  };

  type Props = {
    settings: AppSettings;
    t: Messages;
    capabilities: PlatformCapabilities | null;
    capabilityRows: CapabilityRow[];
    updateChecking: boolean;
    updateStatus: string | undefined;
    updateStatusKind: 'info' | 'error';
    updateReleaseUrl: string | undefined;
    updateDownloadSupported: boolean;
    debounceNumberMs: number;
    scheduleSave: (delay: number) => void;
    runUpdateCheck: () => Promise<void>;
    capabilityStatusLabel: (capability: Capability) => string;
    capabilityDetail: (capability: Capability) => string;
    showSetupButton: (capability: Capability) => boolean;
    onOpenSetup: () => void;
  };

  let {
    settings = $bindable(),
    t,
    capabilities,
    capabilityRows,
    updateChecking,
    updateStatus,
    updateStatusKind,
    updateReleaseUrl,
    updateDownloadSupported,
    debounceNumberMs,
    scheduleSave,
    runUpdateCheck,
    capabilityStatusLabel,
    capabilityDetail,
    showSetupButton,
    onOpenSetup,
  }: Props = $props();
</script>

<fieldset>
  <legend>{t.settings.retention.legend}</legend>
  <label>
    {t.settings.retention.maxBytes}
    <input
      type="number"
      min="0"
      step="1024"
      bind:value={settings.maxEntrySizeBytes}
      oninput={() => scheduleSave(debounceNumberMs)}
    />
  </label>
  <label>
    {t.settings.retention.pasteDelayMs}
    <input
      type="number"
      min="0"
      max="1000"
      step="10"
      bind:value={settings.pasteDelayMs}
      oninput={() => scheduleSave(debounceNumberMs)}
    />
  </label>
</fieldset>
{#if capabilities}
  <fieldset>
    <legend>{t.settings.capabilities.legend}</legend>
    <p class="help">{t.settings.capabilities.help}</p>
    <div class="capability-meta">
      <span><strong>{t.settings.capabilities.platform}:</strong> {capabilities.platform}</span>
      <span><strong>{t.settings.capabilities.tier}:</strong> {capabilities.tier}</span>
    </div>
    <table class="capability-table">
      <thead>
        <tr>
          <th scope="col">{t.settings.capabilities.columns.capability}</th>
          <th scope="col">{t.settings.capabilities.columns.status}</th>
          <th scope="col">{t.settings.capabilities.columns.detail}</th>
        </tr>
      </thead>
      <tbody>
        {#each capabilityRows as row (row.key)}
          <tr>
            <th scope="row" class="capability-label">{row.label}</th>
            <td>
              <span class="capability-status" data-status={row.capability.status}>
                {capabilityStatusLabel(row.capability)}
              </span>
            </td>
            <td class="capability-detail">
              {#if showSetupButton(row.capability)}
                <button type="button" class="capability-setup-link" onclick={onOpenSetup}>
                  {t.settings.capabilities.openSetup}
                </button>
              {:else}
                {capabilityDetail(row.capability)}
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </fieldset>
{/if}
<fieldset>
  <legend>{t.settings.updates.legend}</legend>
  <label>
    <input
      type="checkbox"
      bind:checked={settings.autoUpdateCheck}
      onchange={() => scheduleSave(0)}
    />
    {t.settings.updates.autoCheck}
  </label>
  <p class="help">
    {t.settings.updates.channel}: <strong>{settings.updateChannel}</strong>
  </p>
  <div class="actions">
    <button
      type="button"
      class="secondary compact"
      disabled={updateChecking}
      onclick={runUpdateCheck}
    >
      {updateChecking ? t.settings.updates.checking : t.settings.updates.checkNow}
    </button>
  </div>
  {#if updateStatus}
    <p class="status" class:error={updateStatusKind === 'error'}>
      {updateStatus}
      {#if updateReleaseUrl}
        <a href={updateReleaseUrl} target="_blank" rel="noopener noreferrer">
          {updateDownloadSupported
            ? t.settings.updates.viewRelease
            : t.settings.updates.downloadManual}
        </a>
      {/if}
    </p>
  {/if}
</fieldset>

<style>
  .capability-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem 1.25rem;
    font-size: 0.8125rem;
  }
  .capability-table {
    border-collapse: collapse;
    width: 100%;
    font-size: 0.8125rem;
    table-layout: auto;
  }
  .capability-table th,
  .capability-table td {
    padding: 0.25rem 0.6rem 0.25rem 0;
    text-align: left;
    font-weight: normal;
    vertical-align: baseline;
  }
  .capability-table thead th:nth-child(1),
  .capability-table tbody th {
    white-space: nowrap;
    width: 1%;
  }
  .capability-table thead th:nth-child(2),
  .capability-table tbody td:nth-child(2) {
    white-space: nowrap;
    width: 1%;
  }
  .capability-table thead th {
    font-size: 0.6875rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  }
  .capability-table tbody th {
    font-weight: 500;
  }
  .capability-label {
    color: var(--fg, #f5f5f5);
  }
  .capability-status {
    display: inline-block;
    white-space: nowrap;
    padding: 0.1rem 0.55rem;
    border-radius: 999px;
    font-size: 0.75rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    background: rgba(255, 255, 255, 0.04);
    color: var(--muted, rgba(255, 255, 255, 0.7));
  }
  .capability-status[data-status='available'] {
    color: #4ade80;
    border-color: rgba(74, 222, 128, 0.4);
    background: rgba(74, 222, 128, 0.08);
  }
  .capability-status[data-status='unsupported'] {
    color: var(--danger, #f87171);
    border-color: rgba(248, 113, 113, 0.4);
    background: rgba(248, 113, 113, 0.08);
  }
  .capability-status[data-status='requiresPermission'],
  .capability-status[data-status='requiresExternalTool'],
  .capability-status[data-status='experimental'] {
    color: var(--warning, #f59e0b);
    border-color: rgba(245, 158, 11, 0.4);
    background: rgba(245, 158, 11, 0.08);
  }
  .capability-detail {
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.75rem;
  }
  .capability-setup-link {
    appearance: none;
    background: transparent;
    border: none;
    padding: 0;
    margin: 0;
    color: var(--accent, #60a5fa);
    font: inherit;
    font-size: 0.75rem;
    text-decoration: underline;
    cursor: pointer;
  }
  .capability-setup-link:hover,
  .capability-setup-link:focus-visible {
    color: var(--accent-strong, #93c5fd);
  }
  .capability-setup-link:focus-visible {
    outline: 2px solid var(--accent, #60a5fa);
    outline-offset: 2px;
    border-radius: 2px;
  }
</style>

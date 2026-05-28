<script lang="ts">
  import type { Messages } from '../lib/i18n/locales/en';
  import type { AppSettings, CliInstallStatus } from '../lib/types';

  type Props = {
    settings: AppSettings;
    t: Messages;
    cliStatus: CliInstallStatus | null;
    cliInstalling: boolean;
    cliStatusMessage: string | undefined;
    cliStatusKind: 'info' | 'error';
    scheduleSave: (delay: number) => void;
    runCliInstall: () => Promise<void>;
  };

  let {
    settings,
    t,
    cliStatus,
    cliInstalling,
    cliStatusMessage,
    cliStatusKind,
    scheduleSave,
    runCliInstall,
  }: Props = $props();
</script>

<fieldset>
  <legend>{t.settings.cli.legend}</legend>
  <label>
    <input type="checkbox" bind:checked={settings.cliIpcEnabled} onchange={() => scheduleSave(0)} />
    {t.settings.cli.ipcEnabled}
  </label>
</fieldset>
<fieldset>
  <legend>{t.settings.cli.install.legend}</legend>
  <p class="help">{t.settings.cli.install.help}</p>
  {#if cliStatus}
    {#if !cliStatus.supported}
      <p class="status hint">{t.settings.cli.install.unsupported}</p>
    {:else if !cliStatus.bundled}
      <p class="status hint">{t.settings.cli.install.unavailable}</p>
    {:else}
      <p class="status">
        {cliStatus.installed
          ? t.settings.cli.install.statusInstalled.replace('{path}', cliStatus.installedPath)
          : t.settings.cli.install.statusNotInstalled}
      </p>
      <div class="actions">
        <button
          type="button"
          class="secondary compact"
          disabled={cliInstalling}
          onclick={runCliInstall}
        >
          {#if cliInstalling}
            {t.settings.cli.install.installing}
          {:else if cliStatus.installed}
            {t.settings.cli.install.reinstall}
          {:else}
            {t.settings.cli.install.button}
          {/if}
        </button>
      </div>
      {#if cliStatus.installed && !cliStatus.onPath}
        <p class="status">
          {t.settings.cli.install.notOnPath.replace('{dir}', cliStatus.binDir)}
        </p>
        <pre class="cli-path-export"><code>{t.settings.cli.install.pathExport}</code></pre>
      {/if}
    {/if}
  {/if}
  {#if cliStatusMessage}
    <p class="status" class:error={cliStatusKind === 'error'}>{cliStatusMessage}</p>
  {/if}
</fieldset>

<style>
  .cli-path-export {
    margin: 0.25rem 0 0;
    padding: 0.4rem 0.6rem;
    border-radius: 6px;
    background: var(--surface-muted, rgba(255, 255, 255, 0.06));
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.75rem;
    white-space: pre-wrap;
    word-break: break-all;
    user-select: all;
  }
</style>

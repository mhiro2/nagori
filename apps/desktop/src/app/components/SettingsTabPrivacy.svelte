<script lang="ts">
  import type { Messages } from '../lib/i18n/locales/en';
  import type { UserRegexError } from '../lib/policyValidation';
  import {
    CONTENT_KINDS,
    type AppSettings,
    type Capability,
    type ContentKind,
    type SecretHandling,
  } from '../lib/types';

  type Props = {
    settings: AppSettings;
    t: Messages;
    // Whether the host platform can identify the frontmost app at all.
    // Linux/Wayland reports `unsupported` here, in which case the app
    // denylist would silently drop every capture — render a disabled
    // banner instead of letting the user configure a rule that can't
    // fire.
    frontmostAppCapability: Capability | undefined;
    appDenylistPresetEnabled: boolean;
    appDenylistPatternsText: string;
    regexDenylistText: string;
    regexDenylistErrors: UserRegexError[];
    purgeDeletedBusy: boolean;
    purgeDeletedStatus: string | undefined;
    purgeDeletedStatusKind: 'info' | 'error';
    debounceNumberMs: number;
    debounceTextareaMs: number;
    scheduleSave: (delay: number) => void;
    onPasswordManagerPresetToggle: (next: boolean) => void;
    describeRegexError: (err: UserRegexError) => string;
    toggleCaptureKind: (kind: ContentKind, enabled: boolean) => void;
    runPurgeDeletedEntries: () => Promise<void>;
  };

  let {
    settings = $bindable(),
    t,
    frontmostAppCapability,
    appDenylistPresetEnabled = $bindable(),
    appDenylistPatternsText = $bindable(),
    regexDenylistText = $bindable(),
    regexDenylistErrors,
    purgeDeletedBusy,
    purgeDeletedStatus,
    purgeDeletedStatusKind,
    debounceNumberMs,
    debounceTextareaMs,
    scheduleSave,
    onPasswordManagerPresetToggle,
    describeRegexError,
    toggleCaptureKind,
    runPurgeDeletedEntries,
  }: Props = $props();

  // True on Linux/Wayland where neither X11 nor Wayland exposes the
  // focused window's owning process. Without that the daemon can't tell
  // which app a capture came from, so denylist matching is a no-op.
  // Mirror this state in the UI by disabling the controls and showing
  // a banner so the user knows why the section is inert.
  const denylistDisabled = $derived(frontmostAppCapability?.status === 'unsupported');
</script>

<fieldset>
  <legend>{t.settings.privacy.legend}</legend>
  {#if denylistDisabled}
    <p class="status warning" role="status">
      {t.settings.privacy.appDenylistUnsupported}
    </p>
  {/if}
  <label>
    <input
      type="checkbox"
      checked={appDenylistPresetEnabled}
      disabled={denylistDisabled || undefined}
      onchange={(e) => onPasswordManagerPresetToggle((e.target as HTMLInputElement).checked)}
    />
    {t.settings.privacy.appDenylistPasswordManagers}
  </label>
  <span class="help">{t.settings.privacy.appDenylistPasswordManagersHelp}</span>
  <label class="stack">
    {t.settings.privacy.appDenylistPatterns}
    <textarea
      rows="4"
      bind:value={appDenylistPatternsText}
      disabled={denylistDisabled || undefined}
      oninput={() => scheduleSave(debounceTextareaMs)}
    ></textarea>
    <span class="help">{t.settings.privacy.appDenylistPatternsHelp}</span>
  </label>
  <label class="stack">
    {t.settings.privacy.regexDenylist}
    <textarea
      rows="4"
      bind:value={regexDenylistText}
      oninput={() => scheduleSave(debounceTextareaMs)}
      aria-invalid={regexDenylistErrors.length > 0 || undefined}
      aria-describedby={regexDenylistErrors.length > 0
        ? 'regex-denylist-help regex-denylist-errors regex-denylist-autosave'
        : 'regex-denylist-help regex-denylist-autosave'}
    ></textarea>
    <span class="help" id="regex-denylist-help">
      {t.settings.privacy.regexDenylistHelp}
    </span>
    {#if regexDenylistErrors.length > 0}
      <ul id="regex-denylist-errors" class="status error regex-errors" role="alert">
        {#each regexDenylistErrors as err (`${err.index}:${err.kind}`)}
          <li>
            <strong>
              {t.settings.privacy.regexErrors.lineLabel.replace('{line}', String(err.index + 1))}
            </strong>
            {describeRegexError(err)}
          </li>
        {/each}
      </ul>
      <span class="help" id="regex-denylist-autosave">
        {t.settings.privacy.regexDenylistAutosaveHint}
      </span>
    {:else}
      <span class="help" id="regex-denylist-autosave" hidden></span>
    {/if}
  </label>
  <label class="stack">
    {t.settings.privacy.secretHandling}
    <select
      value={settings.secretHandling}
      onchange={(e) => {
        const select = e.currentTarget as HTMLSelectElement;
        const next = select.value as SecretHandling;
        if (next === 'store_full' && settings.secretHandling !== 'store_full') {
          // Plaintext storage is irreversible against a compromised disk
          // image — gate it behind an explicit confirm so a misclick or
          // muscle memory in a long settings session can't silently flip
          // the durable copy from redacted to raw. The DB has no
          // encryption-at-rest, so the cost of an unintentional toggle is
          // recoverable secrets.
          const ok = window.confirm(t.settings.privacy.storeFullConfirm);
          if (!ok) {
            select.value = settings.secretHandling;
            return;
          }
        }
        settings.secretHandling = next;
        scheduleSave(0);
      }}
    >
      <option value="block">{t.settings.privacy.secretHandlingOptions.block}</option>
      <option value="store_redacted"
        >{t.settings.privacy.secretHandlingOptions.store_redacted}</option
      >
      <option value="store_full">{t.settings.privacy.secretHandlingOptions.store_full}</option>
    </select>
    <span class="help">{t.settings.privacy.secretHandlingHelp}</span>
    {#if settings.secretHandling === 'store_full'}
      <p class="status warning" role="alert">
        {t.settings.privacy.storeFullWarning}
      </p>
    {/if}
  </label>
  <label>
    <input
      type="checkbox"
      bind:checked={settings.blockSensitiveCaptures}
      onchange={() => scheduleSave(0)}
    />
    {t.settings.privacy.blockSensitiveCaptures}
  </label>
  <span class="help">{t.settings.privacy.blockSensitiveCapturesHelp}</span>
  <div class="stack">
    <span>{t.settings.privacy.captureKinds}</span>
    <div class="checkbox-grid">
      {#each CONTENT_KINDS as kind (kind)}
        <label>
          <input
            type="checkbox"
            checked={settings.captureKinds.includes(kind)}
            onchange={(e) => toggleCaptureKind(kind, (e.target as HTMLInputElement).checked)}
          />
          {t.settings.privacy.captureKindOptions[kind]}
        </label>
      {/each}
    </div>
    <span class="help">{t.settings.privacy.captureKindsHelp}</span>
  </div>
</fieldset>

<fieldset>
  <legend>{t.settings.retention.legend}</legend>
  <label>
    {t.settings.retention.maxCount}
    <input
      type="number"
      min="0"
      step="100"
      bind:value={settings.historyRetentionCount}
      oninput={() => scheduleSave(debounceNumberMs)}
    />
  </label>
  <label class="stack">
    {t.settings.retention.maxDays}
    <input
      type="number"
      min="0"
      step="1"
      placeholder={t.settings.retention.maxDaysPlaceholder}
      value={settings.historyRetentionDays ?? 0}
      oninput={(e) => {
        const next = Number((e.target as HTMLInputElement).value);
        settings.historyRetentionDays = Number.isFinite(next) && next > 0 ? next : null;
        scheduleSave(debounceNumberMs);
      }}
    />
    <span class="help">{t.settings.retention.maxDaysHelp}</span>
  </label>
  <label class="stack">
    {t.settings.retention.maxTotalBytes}
    <input
      type="number"
      min="0"
      step="1048576"
      placeholder={t.settings.retention.maxTotalBytesPlaceholder}
      value={settings.maxTotalBytes ?? 0}
      oninput={(e) => {
        const next = Number((e.target as HTMLInputElement).value);
        settings.maxTotalBytes = Number.isFinite(next) && next > 0 ? next : null;
        scheduleSave(debounceNumberMs);
      }}
    />
    <span class="help">{t.settings.retention.maxTotalBytesHelp}</span>
  </label>
  <label>
    <input
      type="checkbox"
      bind:checked={settings.permanentDeleteOnDelete}
      onchange={() => scheduleSave(0)}
    />
    {t.settings.privacy.permanentDeleteOnDelete}
  </label>
  <span class="help">{t.settings.privacy.permanentDeleteOnDeleteHelp}</span>
  <div class="actions">
    <button
      type="button"
      class="secondary compact"
      disabled={purgeDeletedBusy}
      onclick={runPurgeDeletedEntries}
    >
      {purgeDeletedBusy
        ? t.settings.privacy.purgeDeletedRunning
        : t.settings.privacy.purgeDeletedNow}
    </button>
  </div>
  {#if purgeDeletedStatus}
    <p class="status" class:error={purgeDeletedStatusKind === 'error'}>
      {purgeDeletedStatus}
    </p>
  {/if}
</fieldset>

<style>
  .regex-errors {
    margin: 0;
    padding: 0.4rem 0.75rem 0.4rem 1.5rem;
    border: 1px solid var(--danger, #f87171);
    border-radius: 6px;
    background: rgba(248, 113, 113, 0.08);
    font-size: 0.75rem;
    line-height: 1.4;
    list-style: disc;
  }
  .regex-errors li + li {
    margin-top: 0.25rem;
  }
  .regex-errors strong {
    margin-right: 0.35rem;
  }
  .checkbox-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(9rem, 1fr));
    gap: 0.35rem 0.75rem;
  }
</style>

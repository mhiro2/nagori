<script lang="ts">
  import type { Messages } from '../lib/i18n/locales/en';
  import type { UserRegexError } from '../lib/policyValidation';
  import {
    CONTENT_KINDS,
    type AppSettings,
    type ContentKind,
    type SecretHandling,
  } from '../lib/types';

  type Props = {
    settings: AppSettings;
    t: Messages;
    appDenylistText: string;
    regexDenylistText: string;
    regexDenylistErrors: UserRegexError[];
    debounceNumberMs: number;
    debounceTextareaMs: number;
    scheduleSave: (delay: number) => void;
    describeRegexError: (err: UserRegexError) => string;
    toggleCaptureKind: (kind: ContentKind, enabled: boolean) => void;
  };

  let {
    settings = $bindable(),
    t,
    appDenylistText = $bindable(),
    regexDenylistText = $bindable(),
    regexDenylistErrors,
    debounceNumberMs,
    debounceTextareaMs,
    scheduleSave,
    describeRegexError,
    toggleCaptureKind,
  }: Props = $props();
</script>

<fieldset>
  <legend>{t.settings.privacy.legend}</legend>
  <label class="stack">
    {t.settings.privacy.appDenylist}
    <textarea rows="4" bind:value={appDenylistText} oninput={() => scheduleSave(debounceTextareaMs)}
    ></textarea>
    <span class="help">{t.settings.privacy.appDenylistHelp}</span>
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

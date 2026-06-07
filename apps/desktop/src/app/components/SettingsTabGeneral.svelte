<script lang="ts">
  import { LOCALE_PREFERENCES } from '../lib/i18n/index.svelte';
  import type { Messages } from '../lib/i18n/locales/en';
  import { defaultPaletteAccelerator } from '../lib/keybindings';
  import {
    type Appearance,
    type AppSettings,
    type LocaleSetting,
    type PaletteHotkeyAction,
    type PasteFormat,
    type PlatformCapabilities,
    type RecentOrder,
    type SecondaryHotkeyAction,
  } from '../lib/types';
  import HotkeyInput from './HotkeyInput.svelte';

  type Props = {
    settings: AppSettings;
    capabilities: PlatformCapabilities | null;
    hotkeyError: string | undefined;
    t: Messages;
    debounceNumberMs: number;
    paletteHotkeyActions: readonly PaletteHotkeyAction[];
    secondaryHotkeyActions: readonly SecondaryHotkeyAction[];
    scheduleSave: (delay: number) => void;
    clampRowCount: (raw: number) => number;
    onLocaleChange: (next: LocaleSetting) => void;
    onAppearanceChange: (next: Appearance) => void;
    onGlobalHotkeyChange: (next: string) => void;
    onPaletteHotkeyChange: (action: PaletteHotkeyAction, next: string) => void;
    onSecondaryHotkeyChange: (action: SecondaryHotkeyAction, next: string) => void;
  };

  let {
    settings = $bindable(),
    capabilities,
    hotkeyError,
    t,
    debounceNumberMs,
    paletteHotkeyActions,
    secondaryHotkeyActions,
    scheduleSave,
    clampRowCount,
    onLocaleChange,
    onAppearanceChange,
    onGlobalHotkeyChange,
    onPaletteHotkeyChange,
    onSecondaryHotkeyChange,
  }: Props = $props();

  // Fold the action label into an accessible-name / tooltip template
  // (`Restore default for {action}` etc.) the same way the update strings do.
  const sub = (template: string, action: string): string => template.replace('{action}', action);
</script>

<fieldset>
  <legend>{t.settings.capture.legend}</legend>
  <label>
    <input
      type="checkbox"
      bind:checked={settings.captureEnabled}
      onchange={() => scheduleSave(0)}
    />
    {t.settings.capture.enabled}
  </label>
  <label>
    <input
      type="checkbox"
      bind:checked={settings.autoPasteEnabled}
      onchange={() => scheduleSave(0)}
    />
    {t.settings.capture.autoPaste}
  </label>
  <label class="field-row">
    <span>{t.settings.capture.pasteFormatDefault}</span>
    <select
      bind:value={settings.pasteFormatDefault}
      onchange={(e) => {
        settings.pasteFormatDefault = (e.target as HTMLSelectElement).value as PasteFormat;
        scheduleSave(0);
      }}
    >
      <option value="preserve">{t.settings.capture.pasteFormatOptions.preserve}</option>
      <option value="plain_text">{t.settings.capture.pasteFormatOptions.plain_text}</option>
    </select>
  </label>
  <label class="field-row">
    <span>{t.settings.capture.hotkey}</span>
    <HotkeyInput
      value={settings.globalHotkey}
      platform={capabilities?.platform}
      target="tauri-global"
      placeholder={t.settings.hotkeys.placeholder}
      recordingLabel={t.settings.hotkeys.recordingHint}
      recordingCancelHint={t.settings.hotkeys.recordingCancelHint}
      clearLabel={t.settings.hotkeys.clearAriaLabel}
      onChange={onGlobalHotkeyChange}
    />
  </label>
  {#if hotkeyError}
    <p class="status error">{hotkeyError}</p>
  {/if}
  <label class="stack">
    <span>
      <input
        type="checkbox"
        bind:checked={settings.captureInitialClipboardOnLaunch}
        onchange={() => scheduleSave(0)}
      />
      {t.settings.capture.captureInitialClipboard}
    </span>
    <span class="help">{t.settings.capture.captureInitialClipboardHelp}</span>
  </label>
</fieldset>

<fieldset>
  <legend>{t.settings.display.legend}</legend>
  <label class="field-row">
    <span>{t.settings.display.rowCount}</span>
    <input
      type="number"
      min="3"
      max="20"
      step="1"
      value={settings.paletteRowCount}
      oninput={(e) => {
        settings.paletteRowCount = clampRowCount(Number((e.target as HTMLInputElement).value));
        scheduleSave(debounceNumberMs);
      }}
    />
  </label>
  <span class="help">{t.settings.display.rowCountHelp}</span>
  <label class="stack">
    <span>
      <input
        type="checkbox"
        bind:checked={settings.showPreviewPane}
        onchange={() => scheduleSave(0)}
      />
      {t.settings.display.previewPane}
    </span>
    <span class="help">{t.settings.display.previewPaneHelp}</span>
  </label>
</fieldset>

<fieldset>
  <legend>{t.settings.hotkeys.legend}</legend>
  <p class="subhead">{t.settings.hotkeys.paletteHeading}</p>
  <p class="help">{t.settings.hotkeys.paletteHelp}</p>
  <div class="hotkey-list">
    {#each paletteHotkeyActions as action (action)}
      {@const label = t.settings.hotkeys.paletteActions[action]}
      {@const def = defaultPaletteAccelerator(action, capabilities?.platform)}
      <div class="hotkey-row">
        <span class="hotkey-label">{label}</span>
        <HotkeyInput
          value={settings.paletteHotkeys[action] ?? ''}
          platform={capabilities?.platform}
          target="palette-binding"
          variant={def !== null ? 'palette' : 'palette-optional'}
          defaultDisplay={def}
          ariaLabel={sub(t.settings.hotkeys.fieldAriaLabel, label)}
          placeholder={t.settings.hotkeys.placeholder}
          recordingLabel={t.settings.hotkeys.recordingHint}
          recordingCancelHint={t.settings.hotkeys.recordingCancelHint}
          clearLabel={t.settings.hotkeys.clearAriaLabel}
          defaultMarker={t.settings.hotkeys.defaultMarker}
          notSet={t.settings.hotkeys.notSet}
          restoreText={t.settings.hotkeys.reset}
          restoreLabel={sub(t.settings.hotkeys.restoreDefault, label)}
          removeLabel={sub(t.settings.hotkeys.removeShortcut, label)}
          onChange={(next) => onPaletteHotkeyChange(action, next)}
        />
      </div>
    {/each}
  </div>
  <p class="subhead">{t.settings.hotkeys.secondaryHeading}</p>
  <p class="help">{t.settings.hotkeys.secondaryHelp}</p>
  <div class="hotkey-list">
    {#each secondaryHotkeyActions as action (action)}
      {@const label = t.settings.hotkeys.secondaryActions[action]}
      <div class="hotkey-row">
        <span class="hotkey-label">{label}</span>
        <HotkeyInput
          value={settings.secondaryHotkeys[action] ?? ''}
          platform={capabilities?.platform}
          target="tauri-global"
          variant="secondary"
          ariaLabel={sub(t.settings.hotkeys.fieldAriaLabel, label)}
          placeholder={t.settings.hotkeys.placeholder}
          recordingLabel={t.settings.hotkeys.recordingHint}
          recordingCancelHint={t.settings.hotkeys.recordingCancelHint}
          clearLabel={t.settings.hotkeys.clearAriaLabel}
          disabledMarker={t.settings.hotkeys.disabledMarker}
          notSet={t.settings.hotkeys.notSet}
          removeLabel={sub(t.settings.hotkeys.disableShortcut, label)}
          onChange={(next) => onSecondaryHotkeyChange(action, next)}
        />
      </div>
    {/each}
  </div>
</fieldset>

<fieldset>
  <legend>{t.settings.appearance.legend}</legend>
  <label class="field-row">
    <span>{t.settings.appearance.locale}</span>
    <select
      bind:value={settings.locale}
      onchange={(e) => onLocaleChange((e.target as HTMLSelectElement).value as LocaleSetting)}
    >
      {#each LOCALE_PREFERENCES as code (code)}
        <option value={code}>{t.locales[code]}</option>
      {/each}
    </select>
  </label>
  <label class="field-row">
    <span>{t.settings.appearance.theme}</span>
    <select
      bind:value={settings.appearance}
      onchange={(e) => onAppearanceChange((e.target as HTMLSelectElement).value as Appearance)}
    >
      <option value="system">{t.settings.appearance.themeOptions.system}</option>
      <option value="light">{t.settings.appearance.themeOptions.light}</option>
      <option value="dark">{t.settings.appearance.themeOptions.dark}</option>
    </select>
  </label>
  <label class="field-row">
    <span>{t.settings.appearance.recentOrder}</span>
    <select
      bind:value={settings.recentOrder}
      onchange={(e) => {
        settings.recentOrder = (e.target as HTMLSelectElement).value as RecentOrder;
        scheduleSave(0);
      }}
    >
      <option value="by_recency">{t.settings.appearance.recentOrderOptions.by_recency}</option>
      <option value="by_use_count">{t.settings.appearance.recentOrderOptions.by_use_count}</option>
      <option value="pinned_first_then_recency"
        >{t.settings.appearance.recentOrderOptions.pinned_first_then_recency}</option
      >
    </select>
  </label>
</fieldset>

<fieldset>
  <legend>{t.settings.integration.legend}</legend>
  <label>
    <input type="checkbox" bind:checked={settings.autoLaunch} onchange={() => scheduleSave(0)} />
    {t.settings.integration.autoLaunch}
  </label>
  <p class="help">{t.settings.integration.autoLaunchHelp}</p>
  <label>
    <input type="checkbox" bind:checked={settings.showInMenuBar} onchange={() => scheduleSave(0)} />
    {t.settings.integration.menuBar}
  </label>
  <p class="help">{t.settings.integration.menuBarHelp}</p>
  <label>
    <input type="checkbox" bind:checked={settings.clearOnQuit} onchange={() => scheduleSave(0)} />
    {t.settings.integration.clearOnQuit}
  </label>
  <p class="help">{t.settings.integration.clearOnQuitHelp}</p>
</fieldset>

<style>
  /* `label.field-row` layout lives in SettingsView's `.tab-content`
     :global block alongside the other shared form-control rules — see the
     comment there. Scoping it here lost the cascade to that block's
     `label { display: flex }` (equal specificity, defined later), so the
     grid never applied and controls stretched to the full row width. */
  .subhead {
    margin: 0.25rem 0 0;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted, rgba(255, 255, 255, 0.65));
  }
  /* One row per action stacked vertically (label over input). A 2-column
     grid drifted out of alignment once the localized labels varied in width,
     so the layout stays single-column on every viewport for predictable
     localization rather than reviving a label column on wider screens. */
  .hotkey-list {
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
    margin: 0.1rem 0 0.5rem;
  }
  .hotkey-row {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.25rem;
  }
  .hotkey-label {
    font-size: 0.875rem;
  }
</style>

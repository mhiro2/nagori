<script lang="ts">
  import { onMount } from "svelte";
  import { describeError } from "../lib/errors";
  import { checkForUpdates, getSettings, updateSettings } from "../lib/commands";
  import { LOCALE_PREFERENCES, i18nState, messages, setLocale } from "../lib/i18n/index.svelte";
  import { TAURI_EVENTS, isTauri, subscribe } from "../lib/tauri";
  import { applyAppearance } from "../lib/theme";
  import {
    CONTENT_KINDS,
    type AiProviderSetting,
    type Appearance,
    type AppSettings,
    type ContentKind,
    type LocaleSetting,
    type PaletteHotkeyAction,
    type PasteFormat,
    type RecentOrder,
    type SecondaryHotkeyAction,
    type SecretHandling,
  } from "../lib/types";
  import { showPalette } from "../stores/view.svelte";

  type HotkeyFailurePayload = { hotkey: string; error: string };

  type AiProviderTag = "none" | "local" | "remote";
  type Tab = "general" | "privacy" | "ai" | "cli" | "advanced";

  const TABS: readonly Tab[] = ["general", "privacy", "ai", "cli", "advanced"];
  const PALETTE_HOTKEY_ACTIONS: readonly PaletteHotkeyAction[] = [
    "pin",
    "delete",
    "paste-as-plain",
    "copy-without-paste",
    "clear",
    "open-preview",
  ];
  const SECONDARY_HOTKEY_ACTIONS: readonly SecondaryHotkeyAction[] = [
    "repaste-last",
    "clear-history",
  ];

  const providerTag = (value: AiProviderSetting): AiProviderTag =>
    typeof value === "string" ? value : "remote";

  const setProvider = (tag: AiProviderTag): void => {
    if (!settings) return;
    settings.aiProvider = tag === "remote" ? { remote: { name: "openai" } } : tag;
  };

  const onLocaleChange = (next: LocaleSetting): void => {
    if (!settings) return;
    settings.locale = next;
    setLocale(next);
  };

  const onAppearanceChange = (next: Appearance): void => {
    if (!settings) return;
    settings.appearance = next;
    applyAppearance(next);
  };

  const toggleCaptureKind = (kind: ContentKind, enabled: boolean): void => {
    if (!settings) return;
    const next = new Set(settings.captureKinds);
    if (enabled) next.add(kind);
    else next.delete(kind);
    settings.captureKinds = CONTENT_KINDS.filter((candidate) => next.has(candidate));
  };

  // Hotkey override editors store the trimmed accelerator string back onto
  // the settings map; an empty value drops the override so the palette
  // falls back to the default binding declared in `keybindings.ts`.
  const setOverride = <Action extends string, Field extends "paletteHotkeys" | "secondaryHotkeys">(
    field: Field,
    action: Action,
    raw: string,
  ): void => {
    if (!settings) return;
    const value = raw.trim();
    const next: Partial<Record<Action, string>> = {
      ...(settings[field] as Partial<Record<Action, string>>),
    };
    if (value.length === 0) delete next[action];
    else next[action] = value;
    (settings[field] as Partial<Record<Action, string>>) = next;
  };

  const clampRowCount = (raw: number): number => {
    if (!Number.isFinite(raw)) return 8;
    return Math.max(3, Math.min(20, Math.round(raw)));
  };

  // Lists are edited as a single textarea joined by newlines so users can
  // paste sets without juggling individual <input>s.
  const linesToList = (raw: string): string[] =>
    raw
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line.length > 0);

  // Settings live behind the Tauri runtime — `AppSettings::default()` in the
  // backend is the single source of truth, so we render the form only after
  // `get_settings` resolves. In a plain browser (`vite dev`) the call fails
  // and we surface a hint instead of mirroring defaults on the frontend.
  let settings: AppSettings | null = $state(null);
  let activeTab: Tab = $state("general");
  let loading = $state(false);
  let saving = $state(false);
  let error: string | undefined = $state(undefined);
  let appDenylistText = $state("");
  let regexDenylistText = $state("");
  // Populated when the backend fails to register the configured global
  // hotkey at startup or after a save — surfaces the conflict to the user
  // rather than letting the feature silently break.
  let hotkeyError: string | undefined = $state(undefined);

  let updateChecking = $state(false);
  let updateStatus: string | undefined = $state(undefined);
  let updateStatusKind: "info" | "error" = $state("info");
  // Populated when `runUpdateCheck` finds a newer release. The MVP
  // surface is read-only — instead of wiring `download_and_install`
  // we send the user to the published release so they can download
  // the bundle themselves and verify Apple's signature dialog.
  let updateReleaseUrl: string | undefined = $state(undefined);

  // The updater plugin is wired on every OS, but only macOS has a
  // signed `latest.json` feed (Linux ships tarballs without an in-app
  // feed, Windows has no release artefact). See
  // `updater_release_target` in lib.rs. On non-macOS platforms the
  // backend short-circuits with `Unsupported`, so the whole fieldset
  // is hidden rather than rendering controls that would only ever
  // error.
  const isMacOs =
    typeof navigator !== "undefined" && /Mac|iPad|iPhone|iPod/i.test(navigator.userAgent);

  const runUpdateCheck = async (): Promise<void> => {
    if (updateChecking) return;
    updateChecking = true;
    updateStatus = undefined;
    updateReleaseUrl = undefined;
    try {
      const info = await checkForUpdates();
      updateStatusKind = "info";
      if (info) {
        updateStatus = t.settings.updates.available.replace("{version}", info.version);
        // Always-current redirect — never needs to be edited per release.
        updateReleaseUrl = `https://github.com/mhiro2/nagori/releases/tag/v${info.version}`;
      } else {
        updateStatus = t.settings.updates.upToDate;
      }
    } catch (err) {
      updateStatusKind = "error";
      updateStatus = describeError(err);
    } finally {
      updateChecking = false;
    }
  };

  const t = $derived.by(() => {
    void i18nState.locale;
    return messages();
  });

  $effect(() => {
    if (!isTauri()) return;
    loading = true;
    void (async () => {
      try {
        const s = await getSettings();
        settings = s;
        appDenylistText = s.appDenylist.join("\n");
        regexDenylistText = s.regexDenylist.join("\n");
        setLocale(s.locale);
        applyAppearance(s.appearance);
      } catch (err: unknown) {
        error = describeError(err);
      } finally {
        loading = false;
      }
    })();
  });

  const save = async (): Promise<void> => {
    if (!isTauri() || !settings) return;
    settings.appDenylist = linesToList(appDenylistText);
    settings.regexDenylist = linesToList(regexDenylistText);
    saving = true;
    error = undefined;
    hotkeyError = undefined;
    try {
      await updateSettings(settings);
    } catch (err: unknown) {
      error = describeError(err);
    } finally {
      saving = false;
    }
  };

  onMount(() =>
    subscribe<HotkeyFailurePayload>(TAURI_EVENTS.hotkeyRegisterFailed, (payload) => {
      hotkeyError = payload.error || payload.hotkey;
    }),
  );
</script>

<section class="settings">
  <header class="head">
    <h1>{t.settings.title}</h1>
    <button type="button" class="close" onclick={showPalette}>
      {t.settings.backToPalette}
    </button>
  </header>

  {#if loading}
    <p class="status">{t.settings.loading}</p>
  {/if}
  {#if error}
    <p class="status error">{error}</p>
  {/if}

  {#if settings}
    <div class="tabs" role="tablist">
      {#each TABS as tab (tab)}
        <button
          type="button"
          role="tab"
          aria-selected={activeTab === tab}
          class:active={activeTab === tab}
          onclick={() => (activeTab = tab)}
        >
          {t.settings.tabs[tab]}
        </button>
      {/each}
    </div>

    <form
      onsubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
    {#if activeTab === "general"}
      <fieldset>
        <legend>{t.settings.capture.legend}</legend>
        <label>
          <input type="checkbox" bind:checked={settings.captureEnabled} />
          {t.settings.capture.enabled}
        </label>
        <label>
          <input type="checkbox" bind:checked={settings.autoPasteEnabled} />
          {t.settings.capture.autoPaste}
        </label>
        <label>
          {t.settings.capture.pasteFormatDefault}
          <select
            bind:value={settings.pasteFormatDefault}
            onchange={(e) => {
              if (!settings) return;
              settings.pasteFormatDefault = (e.target as HTMLSelectElement).value as PasteFormat;
            }}
          >
            <option value="preserve">{t.settings.capture.pasteFormatOptions.preserve}</option>
            <option value="plain_text">{t.settings.capture.pasteFormatOptions.plain_text}</option>
          </select>
        </label>
        <label>
          {t.settings.capture.hotkey}
          <input type="text" bind:value={settings.globalHotkey} />
        </label>
        {#if hotkeyError}
          <p class="status error">{hotkeyError}</p>
        {/if}
        <label class="stack">
          <span>
            <input type="checkbox" bind:checked={settings.captureInitialClipboardOnLaunch} />
            {t.settings.capture.captureInitialClipboard}
          </span>
          <span class="help">{t.settings.capture.captureInitialClipboardHelp}</span>
        </label>
      </fieldset>

      <fieldset>
        <legend>{t.settings.display.legend}</legend>
        <label>
          {t.settings.display.rowCount}
          <input
            type="number"
            min="3"
            max="20"
            step="1"
            value={settings.paletteRowCount}
            oninput={(e) => {
              if (!settings) return;
              settings.paletteRowCount = clampRowCount(
                Number((e.target as HTMLInputElement).value),
              );
            }}
          />
        </label>
        <span class="help">{t.settings.display.rowCountHelp}</span>
        <label class="stack">
          <span>
            <input type="checkbox" bind:checked={settings.showPreviewPane} />
            {t.settings.display.previewPane}
          </span>
          <span class="help">{t.settings.display.previewPaneHelp}</span>
        </label>
      </fieldset>

      <fieldset>
        <legend>{t.settings.hotkeys.legend}</legend>
        <p class="subhead">{t.settings.hotkeys.paletteHeading}</p>
        <p class="help">{t.settings.hotkeys.paletteHelp}</p>
        <div class="hotkey-grid">
          {#each PALETTE_HOTKEY_ACTIONS as action (action)}
            <label class="hotkey-row">
              <span class="hotkey-label">{t.settings.hotkeys.paletteActions[action]}</span>
              <input
                type="text"
                placeholder={t.settings.hotkeys.placeholder}
                value={settings.paletteHotkeys[action] ?? ""}
                oninput={(e) =>
                  setOverride("paletteHotkeys", action, (e.target as HTMLInputElement).value)}
              />
            </label>
          {/each}
        </div>
        <p class="subhead">{t.settings.hotkeys.secondaryHeading}</p>
        <p class="help">{t.settings.hotkeys.secondaryHelp}</p>
        <div class="hotkey-grid">
          {#each SECONDARY_HOTKEY_ACTIONS as action (action)}
            <label class="hotkey-row">
              <span class="hotkey-label">{t.settings.hotkeys.secondaryActions[action]}</span>
              <input
                type="text"
                placeholder={t.settings.hotkeys.placeholder}
                value={settings.secondaryHotkeys[action] ?? ""}
                oninput={(e) =>
                  setOverride("secondaryHotkeys", action, (e.target as HTMLInputElement).value)}
              />
            </label>
          {/each}
        </div>
      </fieldset>

      <fieldset>
        <legend>{t.settings.appearance.legend}</legend>
        <label>
          {t.settings.appearance.locale}
          <select
            bind:value={settings.locale}
            onchange={(e) => onLocaleChange((e.target as HTMLSelectElement).value as LocaleSetting)}
          >
            {#each LOCALE_PREFERENCES as code (code)}
              <option value={code}>{t.locales[code]}</option>
            {/each}
          </select>
        </label>
        <label>
          {t.settings.appearance.theme}
          <select
            bind:value={settings.appearance}
            onchange={(e) => onAppearanceChange((e.target as HTMLSelectElement).value as Appearance)}
          >
            <option value="system">{t.settings.appearance.themeOptions.system}</option>
            <option value="light">{t.settings.appearance.themeOptions.light}</option>
            <option value="dark">{t.settings.appearance.themeOptions.dark}</option>
          </select>
        </label>
        <label>
          {t.settings.appearance.recentOrder}
          <select
            bind:value={settings.recentOrder}
            onchange={(e) => {
              if (!settings) return;
              settings.recentOrder = (e.target as HTMLSelectElement).value as RecentOrder;
            }}
          >
            <option value="by_recency">{t.settings.appearance.recentOrderOptions.by_recency}</option>
            <option value="by_use_count"
              >{t.settings.appearance.recentOrderOptions.by_use_count}</option
            >
            <option value="pinned_first_then_recency"
              >{t.settings.appearance.recentOrderOptions.pinned_first_then_recency}</option
            >
          </select>
        </label>
      </fieldset>

      <fieldset>
        <legend>{t.settings.integration.legend}</legend>
        <label>
          <input type="checkbox" bind:checked={settings.autoLaunch} />
          {t.settings.integration.autoLaunch}
        </label>
        <p class="help">{t.settings.integration.autoLaunchHelp}</p>
        <label>
          <input type="checkbox" bind:checked={settings.showInMenuBar} />
          {t.settings.integration.menuBar}
        </label>
        <p class="help">{t.settings.integration.menuBarHelp}</p>
        <label>
          <input type="checkbox" bind:checked={settings.clearOnQuit} />
          {t.settings.integration.clearOnQuit}
        </label>
        <p class="help">{t.settings.integration.clearOnQuitHelp}</p>
      </fieldset>
    {/if}

    {#if activeTab === "privacy"}
      <fieldset>
        <legend>{t.settings.privacy.legend}</legend>
        <label>
          <input type="checkbox" bind:checked={settings.localOnlyMode} />
          {t.settings.privacy.localOnly}
        </label>
        <label class="stack">
          {t.settings.privacy.appDenylist}
          <textarea rows="4" bind:value={appDenylistText}></textarea>
          <span class="help">{t.settings.privacy.appDenylistHelp}</span>
        </label>
        <label class="stack">
          {t.settings.privacy.regexDenylist}
          <textarea rows="4" bind:value={regexDenylistText}></textarea>
          <span class="help">{t.settings.privacy.regexDenylistHelp}</span>
        </label>
        <label class="stack">
          {t.settings.privacy.secretHandling}
          <select
            value={settings.secretHandling}
            onchange={(e) => {
              if (!settings) return;
              const select = e.currentTarget as HTMLSelectElement;
              const next = select.value as SecretHandling;
              if (next === "store_full" && settings.secretHandling !== "store_full") {
                // Plaintext storage is irreversible against a compromised
                // disk image — gate it behind an explicit confirm so a
                // misclick or muscle memory in a long settings session
                // can't silently flip the durable copy from redacted to
                // raw. The DB has no encryption-at-rest, so the cost of
                // an unintentional toggle is recoverable secrets.
                const ok = window.confirm(t.settings.privacy.storeFullConfirm);
                if (!ok) {
                  select.value = settings.secretHandling;
                  return;
                }
              }
              settings.secretHandling = next;
            }}
          >
            <option value="block">{t.settings.privacy.secretHandlingOptions.block}</option>
            <option value="store_redacted"
              >{t.settings.privacy.secretHandlingOptions.store_redacted}</option
            >
            <option value="store_full">{t.settings.privacy.secretHandlingOptions.store_full}</option
            >
          </select>
          <span class="help">{t.settings.privacy.secretHandlingHelp}</span>
          {#if settings.secretHandling === "store_full"}
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
                  onchange={(e) =>
                    toggleCaptureKind(kind, (e.target as HTMLInputElement).checked)}
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
              if (!settings) return;
              const next = Number((e.target as HTMLInputElement).value);
              settings.historyRetentionDays = Number.isFinite(next) && next > 0 ? next : null;
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
              if (!settings) return;
              const next = Number((e.target as HTMLInputElement).value);
              settings.maxTotalBytes = Number.isFinite(next) && next > 0 ? next : null;
            }}
          />
          <span class="help">{t.settings.retention.maxTotalBytesHelp}</span>
        </label>
      </fieldset>
    {/if}

    {#if activeTab === "ai"}
      <fieldset>
        <legend>{t.settings.ai.legend}</legend>
        <label>
          <input type="checkbox" bind:checked={settings.aiEnabled} />
          {t.settings.ai.enabled}
        </label>
        <label>
          {t.settings.ai.provider}
          <select
            value={providerTag(settings.aiProvider)}
            onchange={(e) =>
              setProvider((e.target as HTMLSelectElement).value as AiProviderTag)}
          >
            <option value="none">{t.settings.ai.providers.none}</option>
            <option value="local">{t.settings.ai.providers.local}</option>
            <option value="remote">{t.settings.ai.providers.remote}</option>
          </select>
        </label>
        <label>
          <input type="checkbox" bind:checked={settings.semanticSearchEnabled} />
          {t.settings.ai.semanticSearch}
        </label>
      </fieldset>
    {/if}

    {#if activeTab === "cli"}
      <fieldset>
        <legend>{t.settings.cli.legend}</legend>
        <label>
          <input type="checkbox" bind:checked={settings.cliIpcEnabled} />
          {t.settings.cli.ipcEnabled}
        </label>
      </fieldset>
    {/if}

    {#if activeTab === "advanced"}
      <fieldset>
        <legend>{t.settings.retention.legend}</legend>
        <label>
          {t.settings.retention.maxBytes}
          <input
            type="number"
            min="0"
            step="1024"
            bind:value={settings.maxEntrySizeBytes}
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
          />
        </label>
      </fieldset>
      {#if isMacOs}
        <fieldset>
          <legend>{t.settings.updates.legend}</legend>
          <label>
            <input
              type="checkbox"
              bind:checked={settings.autoUpdateCheck}
              disabled={settings.localOnlyMode}
            />
            {t.settings.updates.autoCheck}
          </label>
          <p class="help">
            {settings.localOnlyMode
              ? t.settings.updates.autoCheckLocalOnly
              : t.settings.updates.autoCheckHelp}
          </p>
          <p class="help">
            {t.settings.updates.channel}: <strong>{settings.updateChannel}</strong>
          </p>
          <div class="actions">
            <button type="button" disabled={updateChecking} onclick={runUpdateCheck}>
              {updateChecking ? t.settings.updates.checking : t.settings.updates.checkNow}
            </button>
          </div>
          {#if updateStatus}
            <p class="status" class:error={updateStatusKind === "error"}>
              {updateStatus}
              {#if updateReleaseUrl}
                <a href={updateReleaseUrl} target="_blank" rel="noopener noreferrer">
                  {t.settings.updates.viewRelease}
                </a>
              {/if}
            </p>
          {/if}
        </fieldset>
      {/if}
    {/if}

      <div class="actions">
        <button type="submit" disabled={saving || !isTauri()}>
          {saving ? t.settings.saving : t.settings.save}
        </button>
      </div>
    </form>
  {:else if !loading && !error}
    <p class="status hint">{t.settings.tauriRequired}</p>
  {/if}
</section>

<style>
  .settings {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    height: 100%;
    padding: 1.5rem;
    background: var(--bg, #14161a);
    color: var(--fg, #f5f5f5);
    overflow: auto;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .head h1 {
    margin: 0;
    font-size: 1.125rem;
  }
  .close {
    padding: 0.35rem 0.75rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 6px;
    background: transparent;
    color: inherit;
    cursor: pointer;
  }
  .status {
    color: var(--muted, rgba(255, 255, 255, 0.5));
  }
  .status.error {
    color: var(--danger, #f87171);
  }
  .status.warning {
    margin: 0;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--warning, #f59e0b);
    border-radius: 6px;
    background: rgba(245, 158, 11, 0.08);
    color: var(--warning, #f59e0b);
    font-size: 0.75rem;
    line-height: 1.4;
  }
  .tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  }
  .tabs button {
    padding: 0.45rem 0.9rem;
    border: none;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font: inherit;
    cursor: pointer;
    border-bottom: 2px solid transparent;
  }
  .tabs button.active {
    color: var(--fg, #f5f5f5);
    border-bottom-color: var(--accent, #6c8dff);
  }
  fieldset {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 8px;
    padding: 0.75rem 1rem;
  }
  legend {
    padding: 0 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.875rem;
  }
  label.stack,
  .stack {
    flex-direction: column;
    align-items: stretch;
    gap: 0.35rem;
  }
  .stack {
    display: flex;
    font-size: 0.875rem;
  }
  .checkbox-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(9rem, 1fr));
    gap: 0.35rem 0.75rem;
  }
  .help {
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .subhead {
    margin: 0.25rem 0 0;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted, rgba(255, 255, 255, 0.65));
  }
  .hotkey-grid {
    display: grid;
    grid-template-columns: minmax(11rem, 1fr) 2fr;
    gap: 0.4rem 0.6rem;
  }
  .hotkey-row {
    display: contents;
  }
  .hotkey-label {
    align-self: center;
    font-size: 0.875rem;
  }
  input[type="text"],
  input[type="number"],
  textarea,
  select {
    flex: 1;
    padding: 0.25rem 0.5rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 6px;
    background: var(--bg-elevated, rgba(255, 255, 255, 0.04));
    color: inherit;
    font: inherit;
  }
  textarea {
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    resize: vertical;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .actions button {
    padding: 0.5rem 1.25rem;
    border: 1px solid transparent;
    border-radius: 6px;
    background: var(--accent, #6c8dff);
    color: var(--bg, #14161a);
    font-weight: 600;
    cursor: pointer;
  }
  .actions button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .hint {
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
</style>

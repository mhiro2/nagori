<script lang="ts">
  import { onMount } from "svelte";
  import { getSettings, updateSettings } from "../lib/commands";
  import { SUPPORTED_LOCALES, i18nState, messages, setLocale } from "../lib/i18n/index.svelte";
  import { isTauri } from "../lib/tauri";
  import { applyAppearance } from "../lib/theme";
  import type {
    AiProviderSetting,
    Appearance,
    AppSettings,
    ContentKind,
    LocaleSetting,
    PasteFormat,
    RecentOrder,
    SecretHandling,
  } from "../lib/types";
  import { showPalette } from "../stores/view.svelte";

  type HotkeyFailurePayload = { hotkey: string; error: string };

  type AiProviderTag = "none" | "local" | "remote";
  type Tab = "general" | "privacy" | "ai" | "cli" | "advanced";

  const TABS: readonly Tab[] = ["general", "privacy", "ai", "cli", "advanced"];
  const CAPTURE_KINDS: readonly ContentKind[] = [
    "text",
    "url",
    "code",
    "image",
    "fileList",
    "richText",
    "unknown",
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
    settings.captureKinds = CAPTURE_KINDS.filter((candidate) => next.has(candidate));
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
        error = err instanceof Error ? err.message : String(err);
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
      error = err instanceof Error ? err.message : String(err);
    } finally {
      saving = false;
    }
  };

  onMount(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | undefined;
    void (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<HotkeyFailurePayload>(
        "nagori://hotkey_register_failed",
        (event) => {
          hotkeyError = event.payload.error || event.payload.hotkey;
        },
      );
    })();
    return () => {
      unlisten?.();
    };
  });
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
      </fieldset>

      <fieldset>
        <legend>{t.settings.appearance.legend}</legend>
        <label>
          {t.settings.appearance.locale}
          <select
            bind:value={settings.locale}
            onchange={(e) => onLocaleChange((e.target as HTMLSelectElement).value as LocaleSetting)}
          >
            {#each SUPPORTED_LOCALES as code (code)}
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
            {#each CAPTURE_KINDS as kind (kind)}
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

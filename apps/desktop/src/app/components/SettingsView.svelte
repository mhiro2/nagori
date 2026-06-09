<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import {
    checkForUpdates,
    cliInstallStatus,
    getCapabilities,
    getSettings,
    installCli,
    passwordManagerPreset,
    updateSettings,
  } from '../lib/commands';
  import { describeError, isSettingsConflict } from '../lib/errors';
  import { i18nState, messages, setLocale } from '../lib/i18n/index.svelte';
  import type { Messages } from '../lib/i18n/locales/en';
  import {
    MAX_USER_REGEX_LEN,
    MAX_USER_REGEX_NESTING,
    validateUserRegex,
    type UserRegexError,
  } from '../lib/policyValidation';
  import { SettingsSaveController } from '../lib/settingsSave.svelte';
  import { TAURI_EVENTS, currentWindowLabel, isTauri, subscribe } from '../lib/tauri';
  import { applyAppearance } from '../lib/theme';
  import {
    CONTENT_KINDS,
    type Appearance,
    type AppDenyRule,
    type AppSettings,
    type Capability,
    type CliInstallStatus,
    type ContentKind,
    type LocaleSetting,
    type PaletteHotkeyAction,
    type PlatformCapabilities,
    type SecondaryHotkeyAction,
  } from '../lib/types';
  import SetupRoute from '../routes/SetupRoute.svelte';
  import { refreshCapabilities } from '../stores/capabilities.svelte';
  import { hotkeyFailureState } from '../stores/hotkeyFailure.svelte';
  import { accessibilityGranted, refreshSettings } from '../stores/settings.svelte';
  import { showPalette } from '../stores/view.svelte';
  import SettingsTabAdvanced from './SettingsTabAdvanced.svelte';
  import SettingsTabAi from './SettingsTabAi.svelte';
  import SettingsTabCli from './SettingsTabCli.svelte';
  import SettingsTabGeneral from './SettingsTabGeneral.svelte';
  import SettingsTabPrivacy from './SettingsTabPrivacy.svelte';

  type Tab = 'setup' | 'general' | 'privacy' | 'ai' | 'cli' | 'advanced';

  // Standalone Settings window: the OS supplies the close button via its
  // native title bar, so the in-app "Back to palette" affordance is
  // redundant and hidden. The in-window route (dev/test fallback) still
  // shows the button.
  const isStandaloneSettingsWindow = currentWindowLabel() === 'settings';

  const TABS: readonly Tab[] = ['setup', 'general', 'privacy', 'ai', 'cli', 'advanced'];
  const PALETTE_HOTKEY_ACTIONS: readonly PaletteHotkeyAction[] = [
    'pin',
    'delete',
    'paste-as-plain',
    'copy-without-paste',
    'clear',
    'open-preview',
  ];
  const SECONDARY_HOTKEY_ACTIONS: readonly SecondaryHotkeyAction[] = [
    'repaste-last',
    'clear-history',
  ];

  // Debounce profiles per control class. Checkbox / select edits commit in
  // a single discrete event, so 0 ms keeps the on-disk file in lock-step
  // with the toggle. Free-form text inputs fire `oninput` per keystroke,
  // so a window lets bursts coalesce into one `update_settings` call.
  const DEBOUNCE_NUMBER_MS = 350;
  const DEBOUNCE_TEXTAREA_MS = 500;

  const onLocaleChange = (next: LocaleSetting): void => {
    if (!settings) return;
    settings.locale = next;
    setLocale(next);
    scheduleSave(0);
  };

  const onAppearanceChange = (next: Appearance): void => {
    if (!settings) return;
    settings.appearance = next;
    applyAppearance(next);
    scheduleSave(0);
  };

  const toggleCaptureKind = (kind: ContentKind, enabled: boolean): void => {
    if (!settings) return;
    const next = new Set(settings.captureKinds);
    if (enabled) next.add(kind);
    else next.delete(kind);
    settings.captureKinds = CONTENT_KINDS.filter((candidate) => next.has(candidate));
    scheduleSave(0);
  };

  // Hotkey override editors store the trimmed accelerator string back onto
  // the settings map; an empty value drops the override so the palette
  // falls back to the default binding declared in `keybindings.ts`. State
  // updates fire on every keystroke; the backend round-trip waits for
  // `onblur` so partial accelerator strings ("Cmd+Sh…") don't churn the
  // OS-level shortcut registration.
  const setOverride = <Action extends string, Field extends 'paletteHotkeys' | 'secondaryHotkeys'>(
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

  const onGlobalHotkeyChange = (next: string): void => {
    if (!settings) return;
    settings.globalHotkey = next;
    lastBlurredGlobalHotkey = next;
    scheduleSave(0);
  };

  const onPaletteHotkeyChange = (action: PaletteHotkeyAction, next: string): void => {
    if (!settings) return;
    setOverride('paletteHotkeys', action, next);
    lastBlurredPaletteHotkeys = { ...settings.paletteHotkeys };
    scheduleSave(0);
  };

  const onSecondaryHotkeyChange = (action: SecondaryHotkeyAction, next: string): void => {
    if (!settings) return;
    setOverride('secondaryHotkeys', action, next);
    lastBlurredSecondaryHotkeys = { ...settings.secondaryHotkeys };
    scheduleSave(0);
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

  // Pull free-form `Pattern` rules out for the textarea. Preset rules
  // and non-preset `SourceApp` rules round-trip through their own
  // state slots so the textarea only ever shows what the user can
  // edit there.
  const patternRules = (rules: readonly AppDenyRule[]): string[] =>
    rules.flatMap((rule) => (rule.type === 'pattern' ? [rule.value] : []));

  // Did the loaded list include any preset rule? The toggle defaults
  // to ON if the user has preset rules (initial install or after
  // enabling), OFF if the user explicitly cleared them.
  const presetEnabled = (rules: readonly AppDenyRule[]): boolean =>
    rules.some((rule) => rule.type === 'source_app' && rule.source === 'preset');

  // Extract just the preset-stamped rules so we can cache them as the
  // canonical preset for this Settings session. The dedicated Tauri
  // command is still used as a fallback (e.g. on a fresh install
  // where the user has somehow cleared the preset before opening
  // Settings for the first time), but seeding from the loaded
  // snapshot avoids a hydrate-time race: `save.hydrate` runs
  // synchronously after `getSettings` resolves, and if we waited on
  // `passwordManagerPreset()` instead the first snapshot the
  // controller saw would have an empty preset bucket — a later
  // unrelated save would then ship `appDenylist: []` and silently
  // drop the user's preset rules.
  const extractPresetRules = (rules: readonly AppDenyRule[]): AppDenyRule[] =>
    rules.filter((rule) => rule.type === 'source_app' && rule.source === 'preset');

  // Carry-over bucket: anything that isn't a preset rule and isn't a
  // free-form pattern. Today the UI never produces these (the toggle +
  // textarea cover the visible surface), but the wire format allows
  // typed `SourceApp` rules with `source: 'manual'`, e.g. from a
  // future advanced editor or from an IPC client. Preserve them so a
  // round-trip through Settings doesn't silently delete them.
  const extraRules = (rules: readonly AppDenyRule[]): AppDenyRule[] =>
    rules.filter((rule) => {
      if (rule.type === 'pattern') return false;
      if (rule.type === 'source_app' && rule.source === 'preset') return false;
      return true;
    });

  // Recombine the three UI buckets into the canonical wire shape.
  // Preset rules are pulled from the cached preset list (refreshed
  // each mount) so we always ship the daemon's current source of
  // truth, not whatever stale subset survives in `settings.appDenylist`.
  const assembleDenylist = (): AppDenyRule[] => {
    const out: AppDenyRule[] = [];
    if (appDenylistPresetEnabled) {
      out.push(...passwordManagerPresetRules);
    }
    out.push(...appDenylistExtraRules);
    for (const value of linesToList(appDenylistPatternsText)) {
      out.push({ type: 'pattern', value });
    }
    return out;
  };

  // Toggle handler for the "Block password managers" checkbox. Flips
  // the boolean and schedules a save; `assembleDenylist` adds/removes
  // the preset rules accordingly.
  const onPasswordManagerPresetToggle = (next: boolean): void => {
    appDenylistPresetEnabled = next;
    scheduleSave(0);
  };

  // Settings live behind the Tauri runtime — `AppSettings::default()` in the
  // backend is the single source of truth, so we render the form only after
  // `get_settings` resolves. In a plain browser (`vite dev`) the call fails
  // and we surface a hint instead of mirroring defaults on the frontend.
  let settings: AppSettings | null = $state(null);
  // Static OS capability matrix surfaced read-only in the Advanced tab.
  // Best-effort: failure to load is silently ignored — the section
  // hides rather than spamming the user with a non-actionable error.
  let capabilities: PlatformCapabilities | null = $state(null);
  let activeTab: Tab = $state('general');
  // A tab asked for before the capability snapshot loaded — by the
  // first-launch Setup heuristic or a `navigate` hint. It is parked here
  // and applied once capabilities arrive, so (a) a gated tab never flashes
  // its content before its capability is known (e.g. Windows must not show
  // the macOS Accessibility copy for even a frame), and (b) a hint for a
  // genuinely-visible tab is not dropped just because it raced the probe.
  let requestedTab: Tab | null = $state(null);

  // AI is a per-host capability: the tab only appears where a backend is
  // wired (today macOS). Gating on the capability — not a hardcoded
  // platform — keeps the door open for a future cross-OS provider. Hidden
  // until the snapshot loads so it never flashes on a host that can't run
  // it; a host with no backend never offers a toggle that can't work.
  const aiSupported = $derived.by(() => {
    if (!capabilities) return false;
    return capabilities.aiActions.status !== 'unsupported';
  });
  // Setup hosts only the auto-paste prerequisite card today, so it is worth
  // a tab only where that prerequisite needs user action: the macOS
  // Accessibility grant (`requiresPermission`) or the Linux `wtype` helper
  // (`requiresExternalTool`). Where auto-paste just works (Windows) there is
  // nothing to set up. Capability-gated, not platform-hardcoded, so the tab
  // returns automatically if a host ever grows a real prerequisite. The
  // static capability (not the live grant) is intentional: macOS keeps the
  // tab after granting so the user can revisit / re-grant.
  const setupNeeded = $derived.by(() => {
    if (!capabilities) return false;
    const status = capabilities.autoPaste.status;
    return status === 'requiresPermission' || status === 'requiresExternalTool';
  });
  const visibleTabs = $derived(
    TABS.filter((tab) => {
      if (tab === 'ai') return aiSupported;
      if (tab === 'setup') return setupNeeded;
      return true;
    }),
  );
  // Apply a parked tab request once capabilities are known: switch to it if
  // it proved visible, otherwise drop it (a hidden tab simply leaves the
  // user on the default). This is what makes the deferred Setup heuristic
  // and a raced `navigate` hint land correctly without ever flashing a
  // gated tab's content. Clearing `requestedTab` makes it one-shot, so a
  // later manual tab click is never second-guessed.
  $effect(() => {
    if (requestedTab === null || !capabilities) return;
    const tab = requestedTab;
    requestedTab = null;
    if ((visibleTabs as readonly Tab[]).includes(tab)) activeTab = tab;
  });
  // Bounce off a hidden tab onto General once the snapshot loads — covers an
  // activeTab that was already set to a now-hidden tab through some path
  // other than `requestedTab` (e.g. settings synced from another OS).
  $effect(() => {
    if (!capabilities) return;
    if (!(visibleTabs as readonly Tab[]).includes(activeTab)) activeTab = 'general';
  });
  // Flips true once the initial-tab heuristic has run so a later
  // `onboarding.completedAt` change (e.g. the user clicked through Setup
  // mid-session) does not rip them back to the Setup tab.
  let initialTabResolved = false;
  let loading = $state(false);
  let error: string | undefined = $state(undefined);
  // App-denylist UI is split into three pieces:
  //   - `appDenylistPresetEnabled` mirrors whether any rule with
  //     `source: 'preset'` is in the list (the "Block password managers"
  //     toggle).
  //   - `appDenylistPatternsText` is the multi-line textarea for
  //     free-form `Pattern` rules — only user-typed entries land here.
  //   - `appDenylistExtraRules` holds typed `SourceApp` rules the user
  //     added by some other mechanism (none yet, but the wire format
  //     supports them) plus *manual* `SourceApp` rules; the form
  //     preserves them round-trip so an old config doesn't lose data
  //     just because the UI doesn't surface them.
  // `buildSnapshotPayload` recombines all three into the canonical
  // `AppDenyRule[]` shape at save time.
  let appDenylistPresetEnabled = $state(true);
  let appDenylistPatternsText = $state('');
  let appDenylistExtraRules: AppDenyRule[] = $state([]);
  // Canonical preset shipped by the daemon. Loaded once on mount so the
  // "Block password managers" toggle can add the rules back when the
  // user re-enables it after disabling them. Empty until the IPC
  // resolves; the toggle still works (it just leaves the list empty
  // until the preset arrives, which the user can fix by toggling
  // again).
  let passwordManagerPresetRules: AppDenyRule[] = [];
  let regexDenylistText = $state('');
  // `hydrated` flips true only after `get_settings` resolves *and* the
  // derived textarea state is in sync. Auto-save gates on this flag so
  // the initial render — which assigns `settings`, the app-denylist
  // textarea / toggle state, and `regexDenylistText` in sequence —
  // cannot accidentally feed the defaults straight back to disk.
  let hydrated = $state(false);
  // Mirrors the most recent regex denylist that passed preflight. When
  // the textarea contains a half-typed pattern that fails validation,
  // `buildSnapshotPayload` substitutes this list so a checkbox toggle on
  // General / a hotkey edit elsewhere can still reach disk instead of
  // silently stalling behind the broken Privacy entry.
  let lastValidRegexList: string[] = [];
  // Hotkey controls update live state on every keystroke but only
  // commit on `onblur` — partial accelerators ("Cmd+Sh…") would churn
  // the OS-level shortcut registration. The autosave path runs
  // independently of focus though, so a checkbox toggle elsewhere on
  // the form would otherwise rebuild `buildSnapshotPayload` from live
  // state and leak the partial accelerator into the IPC. Pin the last
  // *blurred* value here and read it — not live state — when assembling
  // the snapshot. The onblur handlers sync the current live value back
  // in before scheduling the save; the unmount flush also syncs so a
  // hotkey edit that never saw `blur` (Escape -> palette tears the
  // input off the DOM) still reaches disk on the way out.
  let lastBlurredGlobalHotkey = '';
  let lastBlurredPaletteHotkeys: Partial<Record<PaletteHotkeyAction, string>> = {};
  let lastBlurredSecondaryHotkeys: Partial<Record<SecondaryHotkeyAction, string>> = {};

  // Compare-and-swap base for `update_settings`: the revision this window last
  // saw via `getSettings` or a `settings_changed` broadcast. It is tracked
  // *outside* the autosave snapshot so it never enters the dedup JSON (a
  // changing revision would otherwise churn the idempotent-IPC guard or loop
  // the autosave). `applyRemoteSettings` advances it as broadcasts arrive —
  // including the echo of our own save — so a tray toggle from the palette
  // refreshes the base before the next save and a conflict only survives the
  // vanishingly small window between dispatch and that broadcast, which the
  // controller's retry then clears.
  let settingsRevision = 0;
  const saveSettings = async (snapshot: AppSettings): Promise<void> => {
    // Inject the live CAS base onto the wire payload. The dedup snapshot pins
    // `revision` to 0 (see `buildSnapshotPayload`); the backend reads this
    // value, not the body's other fields, to detect a stale overwrite.
    try {
      await updateSettings({ ...snapshot, revision: settingsRevision });
    } catch (err: unknown) {
      // A conflict means our baseline is stale. Normally the broadcast that
      // bumped the revision already refreshed `settingsRevision`, but a write
      // landing in the startup attach/hydrate gap can be dropped — leaving the
      // baseline stuck and the autosave retry conflicting forever. Re-fetch the
      // authoritative (settings, revision) pair and reconcile it the same way a
      // broadcast would (preserving in-progress edits and advancing
      // `settingsRevision`), so the controller's follow-up retry compare-and-
      // swaps against reality. Re-throw so the controller still records the
      // failure and schedules that retry.
      if (isSettingsConflict(err)) {
        try {
          applyRemoteSettings(await getSettings());
        } catch {
          // A failed reload leaves the baseline as-is; the next broadcast or a
          // reopen recovers. Fall through to surface the original conflict.
        }
      }
      throw err;
    }
  };

  // Autosave state machine lives in its own module so the textarea
  // debounce, retry timer, in-flight + queued draining, and remote-
  // merge baselines stay testable in isolation. The controller calls
  // back into `buildSnapshotPayload` whenever it needs a fresh payload
  // so the snapshot still composes live form state with the pinned
  // `lastBlurred…` set.
  const save = new SettingsSaveController({
    buildSnapshot: () => buildSnapshotPayload(),
    updateSettings: saveSettings,
    describeError,
    onSaveSuccess: () => {
      error = undefined;
    },
  });
  const scheduleSave = (delay: number): void => {
    save.scheduleSave(delay);
  };
  // Live preflight against the same limits `compile_user_regex` enforces in
  // `nagori-core::policy`. Rendered inline next to the textarea so the user
  // sees per-line guidance ("too long", "nested too deep", "invalid syntax")
  // before the daemon would otherwise reject the save. The validator's
  // `index` is set to the textarea's 1-based row number minus one so the
  // rendered `Line N` label matches the row the user is editing, even when
  // blank lines sit between entries.
  let regexDenylistErrors = $derived.by<UserRegexError[]>(() =>
    regexDenylistText.split(/\r?\n/).flatMap((line, idx) => {
      const trimmed = line.trim();
      if (trimmed.length === 0) return [];
      const err = validateUserRegex(trimmed, idx);
      return err ? [err] : [];
    }),
  );
  // Populated when the backend fails to register the configured global
  // hotkey at startup or after a save — surfaces the conflict to the user
  // rather than letting the feature silently break. Driven by the shared
  // App-level store so a startup-time failure (emitted before this view
  // mounted) is still visible after the user opens Settings later.
  const hotkeyError = $derived.by<string | undefined>(() => {
    const failure = hotkeyFailureState.failure;
    if (!failure) return undefined;
    return failure.error || failure.hotkey || undefined;
  });

  let updateChecking = $state(false);
  let updateStatus: string | undefined = $state(undefined);
  let updateStatusKind: 'info' | 'error' = $state('info');
  // Populated when `runUpdateCheck` finds a newer release. The MVP
  // surface is read-only — instead of wiring `download_and_install`
  // we send the user to the published release so they can download
  // the bundle themselves and verify Apple's signature dialog.
  let updateReleaseUrl: string | undefined = $state(undefined);

  let updateDownloadSupported = $state(true);

  const runUpdateCheck = async (): Promise<void> => {
    if (updateChecking) return;
    updateChecking = true;
    updateStatus = undefined;
    updateReleaseUrl = undefined;
    try {
      const info = await checkForUpdates();
      updateStatusKind = 'info';
      if (info) {
        updateDownloadSupported = info.downloadSupported;
        // Whether the install medium supports in-place replacement
        // decides the wording: AppImage/NSIS/.app can swap the bundle
        // automatically, a `.deb` install needs the user to fetch a
        // new package manually. We always link to the GitHub release
        // page; the difference is the surrounding copy.
        updateStatus = info.downloadSupported
          ? t.settings.updates.available.replace('{version}', info.version)
          : t.settings.updates.availableManual.replace('{version}', info.version);
        // Always-current redirect — never needs to be edited per release.
        updateReleaseUrl = `https://github.com/mhiro2/nagori/releases/tag/v${info.version}`;
      } else {
        updateStatus = t.settings.updates.upToDate;
      }
    } catch (err) {
      updateStatusKind = 'error';
      updateStatus = describeError(err);
    } finally {
      updateChecking = false;
    }
  };

  // Read-only state of the bundled `nagori` CLI, loaded when the CLI tab is
  // first shown. `null` while unknown; the install affordance only renders
  // once a status is available.
  let cliStatus: CliInstallStatus | null = $state(null);
  let cliInstalling = $state(false);
  let cliStatusMessage: string | undefined = $state(undefined);
  let cliStatusKind: 'info' | 'error' = $state('info');

  const loadCliStatus = async (): Promise<void> => {
    try {
      cliStatus = await cliInstallStatus();
    } catch {
      // Diagnostic-only surface; a failure just hides the install affordance.
      cliStatus = null;
    }
  };

  const runCliInstall = async (): Promise<void> => {
    if (cliInstalling) return;
    cliInstalling = true;
    cliStatusMessage = undefined;
    try {
      const result = await installCli();
      cliStatusKind = 'info';
      cliStatusMessage = result.onPath
        ? t.settings.cli.install.installed.replace('{path}', result.installedPath)
        : t.settings.cli.install.installedNeedsPath.replace('{path}', result.installedPath);
      // Refresh so the button flips to its "installed" affordance.
      await loadCliStatus();
    } catch (err) {
      cliStatusKind = 'error';
      cliStatusMessage = describeError(err);
    } finally {
      cliInstalling = false;
    }
  };

  const t = $derived.by(() => {
    void i18nState.locale;
    return messages();
  });

  // Lazily probe CLI install state the first time the CLI tab is shown. The
  // probe spawns the user's login shell to read PATH, so we avoid running it
  // on every Settings open by gating on the active tab.
  $effect(() => {
    if (isTauri() && activeTab === 'cli' && cliStatus === null) {
      void loadCliStatus();
    }
  });

  $effect(() => {
    if (!isTauri()) return;
    loading = true;
    void (async () => {
      try {
        const s = await getSettings();
        settings = s;
        // Seed the compare-and-swap base from the freshly loaded snapshot.
        settingsRevision = s.revision ?? 0;
        // First-launch heuristic: surface the Setup tab when the user has
        // never reached a successful Accessibility grant. Today the daemon
        // only stamps `accessibilityPromptedAt` / `accessibilityFirstGrantedAt`
        // — `completedAt` is reserved for a future explicit dismissal — so we
        // gate on both fields rather than `completedAt` alone (otherwise every
        // launch lands on Setup even after the user is fully onboarded).
        // Only runs once per Settings session so we never override an
        // explicit tab click later in the same window. We *request* Setup
        // rather than switch to it directly: the resolver applies it only
        // once capabilities confirm Setup is visible, so a host with nothing
        // to set up (Windows) never even briefly renders the card.
        if (!initialTabResolved) {
          initialTabResolved = true;
          if (
            s.onboarding.completedAt === null &&
            s.onboarding.accessibilityFirstGrantedAt === null
          ) {
            requestedTab = 'setup';
          }
        }
        appDenylistPresetEnabled = presetEnabled(s.appDenylist);
        appDenylistExtraRules = extraRules(s.appDenylist);
        appDenylistPatternsText = patternRules(s.appDenylist).join('\n');
        // Seed the preset cache from the loaded settings so
        // `assembleDenylist` can faithfully re-emit the preset rules
        // *before* the async `passwordManagerPreset()` call resolves.
        // Without this seed an unrelated save firing between hydrate
        // and the IPC completing would persist an empty `appDenylist`
        // because `assembleDenylist` would have no preset rules to
        // splice in.
        const loadedPreset = extractPresetRules(s.appDenylist);
        if (loadedPreset.length > 0) {
          passwordManagerPresetRules = loadedPreset;
        }
        regexDenylistText = s.regexDenylist.join('\n');
        lastValidRegexList = [...s.regexDenylist];
        lastBlurredGlobalHotkey = s.globalHotkey;
        lastBlurredPaletteHotkeys = { ...s.paletteHotkeys };
        lastBlurredSecondaryHotkeys = { ...s.secondaryHotkeys };
        setLocale(s.locale);
        applyAppearance(s.appearance);
        // All form-bound state is now in sync with the backend snapshot;
        // arming `hydrated` here means handlers fired during the initial
        // bindings (e.g. Svelte's two-way binding pass) cannot trigger
        // a spurious save. The controller mirrors this gate so its own
        // `scheduleSave` / `commitSave` short-circuit until the
        // initial-baseline seed has run.
        hydrated = true;
        // The freshly-loaded form already matches what's on disk, so
        // seed both baselines from the same snapshot. This suppresses a
        // no-op save on the first commit after hydration and keeps the
        // unmount flush quiet when the user only opened Settings to
        // read.
        save.hydrate(JSON.stringify(buildSnapshotPayload()));
      } catch (err: unknown) {
        error = describeError(err);
      } finally {
        loading = false;
      }
    })();
    void (async () => {
      try {
        capabilities = await getCapabilities();
      } catch {
        // Diagnostic-only surface; ignore failures.
      }
    })();
    void (async () => {
      try {
        const fromIpc = await passwordManagerPreset();
        // Only adopt the IPC result if we don't already have a preset
        // seeded from the loaded settings — the load path runs
        // synchronously after `getSettings` resolves and is the
        // canonical source for the *user's current* preset (they
        // may have removed individual rules). The IPC's role is the
        // fresh-install fallback when the loaded settings have no
        // preset rules at all.
        if (passwordManagerPresetRules.length === 0) {
          passwordManagerPresetRules = fromIpc;
        }
      } catch {
        // Best-effort: when the preset call fails we leave the cache
        // at whatever the load seeded (possibly empty). Toggling the
        // preset back on in that state writes an empty preset
        // section until the next Settings open; logging would just
        // noise the diagnostics surface for a never-seen failure
        // mode (the command never touches disk).
      }
    })();
    // The Settings window is a separate Tauri webview, so the palette's
    // mount-time `refreshSettings` never runs here. Without this fetch the
    // capability table reads `accessibilityGranted() === false` even when
    // the user has actually granted the permission, and the Auto-paste
    // row sticks on "Needs permission" while auto-paste itself works.
    void refreshSettings();
    // PermissionCard reads `capabilitiesState` to drive the macOS
    // screenshot and the non-macOS short-circuit in the UI resolver. The
    // standalone Settings webview never mounts the Palette so the shared
    // store stays empty otherwise — populate it explicitly here.
    void refreshCapabilities();
  });

  type CapabilityRowKey = keyof Messages['settings']['capabilities']['rows'];

  type CapabilityRow = {
    key: CapabilityRowKey;
    label: string;
    capability: Capability;
  };

  // The backend capability matrix is static ("what the OS could do, given
  // a permission") — it does not know whether the user has actually
  // granted Accessibility. Merge the live `PermissionChecker` result here
  // so a granted permission flips the row from "Needs permission" to
  // "Available", matching what the user observes when Enter triggers a
  // real paste.
  const resolveCapability = (cap: Capability): Capability => {
    if (
      cap.status === 'requiresPermission' &&
      cap.permission === 'accessibility' &&
      accessibilityGranted()
    ) {
      return { status: 'available' };
    }
    return cap;
  };

  const capabilityRows = $derived.by<CapabilityRow[]>(() => {
    if (!capabilities) return [];
    const rows = t.settings.capabilities.rows;
    return [
      { key: 'captureText', label: rows.captureText, capability: capabilities.captureText },
      { key: 'captureImage', label: rows.captureImage, capability: capabilities.captureImage },
      { key: 'captureFiles', label: rows.captureFiles, capability: capabilities.captureFiles },
      { key: 'writeText', label: rows.writeText, capability: capabilities.writeText },
      { key: 'writeImage', label: rows.writeImage, capability: capabilities.writeImage },
      {
        key: 'clipboardMultiRepresentationWrite',
        label: rows.clipboardMultiRepresentationWrite,
        capability: capabilities.clipboardMultiRepresentationWrite,
      },
      {
        key: 'autoPaste',
        label: rows.autoPaste,
        capability: resolveCapability(capabilities.autoPaste),
      },
      { key: 'globalHotkey', label: rows.globalHotkey, capability: capabilities.globalHotkey },
      { key: 'frontmostApp', label: rows.frontmostApp, capability: capabilities.frontmostApp },
      { key: 'permissionsUi', label: rows.permissionsUi, capability: capabilities.permissionsUi },
      { key: 'updateCheck', label: rows.updateCheck, capability: capabilities.updateCheck },
      {
        key: 'previewQuickLook',
        label: rows.previewQuickLook,
        capability: capabilities.previewQuickLook,
      },
    ];
  });

  const capabilityStatusLabel = (capability: Capability): string => {
    const statuses = t.settings.capabilities.statuses;
    switch (capability.status) {
      case 'available':
        return statuses.available;
      case 'unsupported':
        return statuses.unsupported;
      case 'requiresPermission':
        return statuses.requiresPermission;
      case 'requiresExternalTool':
        return statuses.requiresExternalTool;
      case 'experimental':
        return statuses.experimental;
    }
  };

  const capabilityDetail = (capability: Capability): string => {
    switch (capability.status) {
      case 'available':
        return '';
      case 'unsupported':
        return capability.reason;
      case 'requiresPermission':
      case 'requiresExternalTool':
        // Detail text is intentionally empty for these statuses. The Setup
        // tab carries the localised description, screenshot, and CTA so the
        // capability row stays a single-glance diagnostic readout and the
        // `[Open Setup]` button (rendered alongside this cell) is the only
        // affordance on the row.
        return '';
      case 'experimental':
        return capability.message;
    }
  };

  // True for rows whose remediation lives on the Setup tab. The detail cell
  // renders an `Open Setup` button instead of free-form text, switching the
  // active tab in the same Settings window.
  const showSetupButton = (capability: Capability): boolean =>
    capability.status === 'requiresPermission' || capability.status === 'requiresExternalTool';

  // Assemble the settings payload from the current form state. Splitting
  // the payload out from the JSON round-trip lets `commitSave` and
  // `onDestroy` compare candidate snapshots against `lastSentJson` /
  // `lastPersistedJson` without cloning first. Textarea -> list
  // flattening happens here (not on every keystroke) so intermediate
  // states like a trailing blank line don't reshape the stored array.
  // While the regex textarea is mid-edit and fails preflight we
  // substitute the last valid list so a checkbox toggle on General
  // isn't held hostage by a half-typed pattern on Privacy. Hotkey
  // fields read from the pinned `lastBlurred…` set rather than live
  // state so the "no save until blur" contract survives an unrelated
  // autosave firing mid-typing.
  const buildSnapshotPayload = (): AppSettings => ({
    ...(settings as AppSettings),
    globalHotkey: lastBlurredGlobalHotkey,
    paletteHotkeys: lastBlurredPaletteHotkeys,
    secondaryHotkeys: lastBlurredSecondaryHotkeys,
    appDenylist: assembleDenylist(),
    regexDenylist:
      regexDenylistErrors.length === 0 ? linesToList(regexDenylistText) : lastValidRegexList,
    // Revision is the compare-and-swap base, tracked separately and passed to
    // `update_settings` by `saveSettings`. Pin it to a constant here so it
    // never enters the autosave dedup JSON — a live revision spread from
    // `settings` (or echoed by a broadcast) would churn the idempotent-IPC
    // guard and could loop the autosave. The backend ignores the body's
    // revision and reads the explicit argument instead.
    revision: 0,
  });

  // Promote each clean version of the regex textarea to the "last valid"
  // baseline. `commitSave` and the unmount flush read this when the
  // current textarea fails preflight so other-tab edits still ship the
  // most recent regex set the backend successfully accepted.
  $effect(() => {
    if (!hydrated) return;
    if (regexDenylistErrors.length === 0) {
      lastValidRegexList = linesToList(regexDenylistText);
    }
  });

  const describeRegexError = (err: UserRegexError): string => {
    const regexMessages = t.settings.privacy.regexErrors;
    switch (err.kind) {
      case 'too_long':
        return regexMessages.tooLong
          .replace('{bytes}', String(err.detail.byteLength ?? 0))
          .replace('{limit}', String(MAX_USER_REGEX_LEN));
      case 'too_nested':
        return regexMessages.tooNested
          .replace('{depth}', String(err.detail.nesting ?? 0))
          .replace('{limit}', String(MAX_USER_REGEX_NESTING));
      case 'invalid_syntax':
        return regexMessages.invalidSyntax.replace('{error}', err.detail.syntaxError ?? '');
      case 'empty':
        return regexMessages.empty;
    }
  };

  const saveStatusLabel = $derived.by(() => {
    switch (save.saveStatus) {
      case 'saving':
        return t.settings.statusSaving;
      case 'saved':
        return t.settings.statusSaved;
      case 'error':
        return t.settings.statusError.replace('{error}', save.saveError ?? '');
      case 'idle':
        return '';
    }
  });

  // Order-independent value equality for the leaf shapes that show up
  // inside `AppSettings`: primitives compare by `===`, arrays element-
  // wise, and plain objects (palette/secondary hotkey records, the
  // `{ remote: { name } }` AI-provider variant) by key set + recursive
  // value compare. `paletteHotkeys` and friends are serialized from a
  // Rust BTreeMap (sorted-key order) but `setOverride` re-spreads them
  // locally so the key order can drift between local and remote —
  // comparing by `JSON.stringify` would then mis-classify a structurally
  // identical record as "user edited" and drop the remote update.
  const fieldEqual = (a: unknown, b: unknown): boolean => {
    if (a === b) return true;
    if (a === null || b === null) return false;
    if (Array.isArray(a) || Array.isArray(b)) {
      if (!Array.isArray(a) || !Array.isArray(b)) return false;
      if (a.length !== b.length) return false;
      for (let i = 0; i < a.length; i += 1) {
        if (!fieldEqual(a[i], b[i])) return false;
      }
      return true;
    }
    if (typeof a !== 'object' || typeof b !== 'object') return false;
    const ao = a as Record<string, unknown>;
    const bo = b as Record<string, unknown>;
    const ak = Object.keys(ao);
    const bk = Object.keys(bo);
    if (ak.length !== bk.length) return false;
    for (const k of ak) {
      if (!Object.hasOwn(bo, k)) return false;
      if (!fieldEqual(ao[k], bo[k])) return false;
    }
    return true;
  };

  // Merge a backend-published `settings_changed` snapshot into the in-
  // memory view. Each top-level field is treated independently: a field
  // still equal to the last persisted baseline has not been touched
  // locally since the last sync — adopt remote's value. Any divergent
  // field is the user's in-progress edit; keep it so the next autosave
  // still flushes their change. Without this merge, an external mutation
  // (tray's "Pause Capture" toggle, another window, an IPC client) is
  // silently overwritten the next time SettingsView autosaves the full
  // snapshot.
  const applyRemoteSettings = (remote: AppSettings): void => {
    if (!hydrated || !settings) return;
    // Advance the compare-and-swap base to whatever the backend just
    // published — this is what keeps a tray toggle (or another client's save)
    // from leaving our baseline stale, so the next autosave compare-and-swaps
    // against reality instead of conflicting. The revision is dropped from the
    // dedup JSON below (pinned to 0, matching `buildSnapshotPayload`) so the
    // echo/merge comparison stays revision-agnostic.
    settingsRevision = remote.revision ?? settingsRevision;
    const remoteJson = JSON.stringify({ ...remote, revision: 0 });
    // Echo of our own most-recent dispatch — the backend has confirmed
    // the write landed. Advance the persisted baseline so subsequent
    // remote events evaluate against reality, but leave local state
    // alone: anything the user has typed since we sent the snapshot
    // stays as an unflushed edit.
    if (save.noteEcho(remoteJson)) return;
    const baseline = JSON.parse(save.persistedJson) as AppSettings;
    const local = settings;
    // Denylist fields live in textarea / toggle state, not in
    // `settings` directly — `settings.appDenylist` only catches up
    // when `buildSnapshotPayload` runs at save time. Compute the
    // dirty flag against the *baseline JSON* representation so a
    // mid-typed pattern (or a freshly-flipped toggle) is preserved
    // when an external save lands.
    const baselineAppPatterns = patternRules(baseline.appDenylist).join('\n');
    const baselinePresetEnabled = presetEnabled(baseline.appDenylist);
    const baselineRegexText = baseline.regexDenylist.join('\n');
    const appDenylistDirty =
      appDenylistPatternsText !== baselineAppPatterns ||
      appDenylistPresetEnabled !== baselinePresetEnabled ||
      !fieldEqual(appDenylistExtraRules, extraRules(baseline.appDenylist));
    const regexDenylistDirty = regexDenylistText !== baselineRegexText;
    type Key = keyof AppSettings;
    for (const key of Object.keys(remote) as Key[]) {
      let dirty: boolean;
      if (key === 'appDenylist') {
        dirty = appDenylistDirty;
      } else if (key === 'regexDenylist') {
        dirty = regexDenylistDirty;
      } else {
        dirty = !fieldEqual(local[key], baseline[key]);
      }
      if (!dirty) {
        (local as unknown as Record<string, unknown>)[key] = remote[key] as unknown;
      }
    }
    // Re-derive UI state for adopted fields. Reuse the dirty flags from
    // the loop above so a textarea we just classified as user-edited
    // keeps its in-progress content (and `lastValidRegexList`) intact.
    if (!appDenylistDirty) {
      appDenylistPresetEnabled = presetEnabled(remote.appDenylist);
      appDenylistExtraRules = extraRules(remote.appDenylist);
      appDenylistPatternsText = patternRules(remote.appDenylist).join('\n');
    }
    if (!regexDenylistDirty) {
      regexDenylistText = remote.regexDenylist.join('\n');
      lastValidRegexList = [...remote.regexDenylist];
    } else if (regexDenylistErrors.length > 0) {
      // User is mid-edit on an invalid pattern. `buildSnapshotPayload`
      // substitutes `lastValidRegexList` when the textarea fails the
      // preflight; without a sync here the next autosave would ship the
      // stale pre-merge regex list and silently clobber the just-merged
      // remote value. For the dirty+valid case the `$effect` below has
      // already promoted the user's textarea to `lastValidRegexList` so
      // skipping it here preserves their unsaved intent.
      lastValidRegexList = [...remote.regexDenylist];
    }
    if (local.globalHotkey === remote.globalHotkey) {
      lastBlurredGlobalHotkey = remote.globalHotkey;
    }
    if (fieldEqual(local.paletteHotkeys, remote.paletteHotkeys)) {
      lastBlurredPaletteHotkeys = { ...remote.paletteHotkeys };
    }
    if (fieldEqual(local.secondaryHotkeys, remote.secondaryHotkeys)) {
      lastBlurredSecondaryHotkeys = { ...remote.secondaryHotkeys };
    }
    if (local.locale === remote.locale) setLocale(remote.locale);
    if (local.appearance === remote.appearance) applyAppearance(remote.appearance);

    // Backend is now authoritative at `remote`. The controller updates
    // its baselines and cancels/reschedules any pending retry so a
    // stale failed-payload retry can't silently undo this merge. When
    // a save is in flight the controller raises an
    // `externalMergeDuringInflight` flag and the `finally` hook fires
    // the follow-up commit — using `remoteJson` instead of
    // `buildSnapshotPayload()` keeps preserved-dirty fields intact for
    // that follow-up.
    save.noteExternalMerge(remoteJson);
  };

  onMount(() => {
    // Hotkey-failure subscription has moved to App.svelte (always-on,
    // also re-hydrates from the backend's cached snapshot via
    // `last_hotkey_failure`). This view now derives `hotkeyError` from
    // `hotkeyFailureState`, so opening Settings after a startup-time
    // failure still shows the conflict.
    const offSettings = subscribe<AppSettings>(TAURI_EVENTS.settingsChanged, applyRemoteSettings);
    // Tab-hint listener. `open_settings` re-emits its `route` argument
    // on the Settings webview after showing the window so a caller that
    // already knows where the user needs to land (e.g. the Palette
    // accessibility indicator) can jump straight there instead of
    // waiting on the first-launch heuristic — which would land on
    // General once `accessibilityFirstGrantedAt` is stamped, even if
    // the grant has since been revoked.
    const offNavigate = subscribe<string>(TAURI_EVENTS.navigate, (payload) => {
      if (typeof payload !== 'string') return;
      const tab = payload as Tab;
      // Accept any known tab; visibility is enforced by the resolver. Route
      // through `requestedTab` rather than asserting `activeTab` here so a
      // hint that beats the capability probe (e.g. the StatusBar Setup
      // affordance clicked right as Settings opens) is not silently dropped
      // — it applies the moment the snapshot lands.
      if ((TABS as readonly string[]).includes(tab)) {
        requestedTab = tab;
        // Mark the initial-tab heuristic resolved so a later
        // `getSettings` callback inside the same Settings session does
        // not snap the user back to its default selection.
        initialTabResolved = true;
      }
    });
    return () => {
      offSettings();
      offNavigate();
    };
  });

  onDestroy(() => {
    // Flush any in-memory edits the user hasn't given the debounce / blur
    // path a chance to commit. Covers three loss paths: a textarea or
    // number input still inside its debounce window when the user
    // navigates away, a queued snapshot waiting on the post-commit drain
    // (won't fire once the controller is destroyed), and a hotkey field
    // that was edited via `setOverride` but never blurred (Escape ->
    // palette tears the focused input off the DOM without firing `blur`).
    if (hydrated && settings) {
      // Promote any unblurred hotkey edits into the pinned set before
      // building the snapshot — unmount is the user's last chance, so
      // accept whatever the live state holds (the normal autosave path
      // would skip it). Without this the snapshot would carry the
      // previously-blurred value and the unblurred edit would silently
      // vanish — see "flushes a hotkey edit on unmount even without a
      // blur event".
      lastBlurredGlobalHotkey = settings.globalHotkey;
      lastBlurredPaletteHotkeys = { ...settings.paletteHotkeys };
      lastBlurredSecondaryHotkeys = { ...settings.secondaryHotkeys };
      const snapshotJson = JSON.stringify(buildSnapshotPayload());
      const snapshot = JSON.parse(snapshotJson) as AppSettings;
      save.flushOnUnmount(snapshotJson, snapshot);
    } else {
      // Pre-hydration teardown: nothing to flush, but timers from a
      // debounced edit attempt (or an `error`-branch retry timer that
      // armed against a partial load) still need cancelling.
      save.flushOnUnmount('', {} as AppSettings);
    }
  });
</script>

<section class="settings">
  <header class="head">
    <h1>{t.settings.title}</h1>
    <div class="head-trailing">
      {#if save.saveStatus !== 'idle'}
        <span class="save-status" data-status={save.saveStatus} aria-live="polite">
          {saveStatusLabel}
        </span>
      {/if}
      {#if !isStandaloneSettingsWindow}
        <button type="button" class="close" onclick={showPalette}>
          {t.settings.backToPalette}
        </button>
      {/if}
    </div>
  </header>

  {#if loading}
    <p class="status">{t.settings.loading}</p>
  {/if}
  {#if error}
    <p class="status error">{error}</p>
  {/if}

  {#if settings}
    <div class="tabs" role="tablist">
      {#each visibleTabs as tab (tab)}
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

    <form onsubmit={(e) => e.preventDefault()}>
      {#if activeTab === 'setup'}
        <SetupRoute />
      {:else}
        <!--
          Wrapper that anchors the shared form-control CSS to the non-Setup
          tabs only. `:global(...)` selectors below traverse into the tab
          child components, so anchoring on `.settings` would also reach
          into `PermissionCard` (rendered by SetupRoute) and override its
          button styling. The setup branch never renders this wrapper, so
          its scoped CSS stays isolated.
        -->
        <div class="tab-content">
          {#if activeTab === 'general'}
            <SettingsTabGeneral
              bind:settings
              {capabilities}
              {hotkeyError}
              {t}
              debounceNumberMs={DEBOUNCE_NUMBER_MS}
              paletteHotkeyActions={PALETTE_HOTKEY_ACTIONS}
              secondaryHotkeyActions={SECONDARY_HOTKEY_ACTIONS}
              {scheduleSave}
              {clampRowCount}
              {onLocaleChange}
              {onAppearanceChange}
              {onGlobalHotkeyChange}
              {onPaletteHotkeyChange}
              {onSecondaryHotkeyChange}
            />
          {/if}

          {#if activeTab === 'privacy'}
            <SettingsTabPrivacy
              bind:settings
              {t}
              frontmostAppCapability={capabilities?.frontmostApp}
              bind:appDenylistPresetEnabled
              bind:appDenylistPatternsText
              bind:regexDenylistText
              {regexDenylistErrors}
              debounceNumberMs={DEBOUNCE_NUMBER_MS}
              debounceTextareaMs={DEBOUNCE_TEXTAREA_MS}
              {scheduleSave}
              {onPasswordManagerPresetToggle}
              {describeRegexError}
              {toggleCaptureKind}
            />
          {/if}

          {#if activeTab === 'ai'}
            <SettingsTabAi bind:settings {t} {scheduleSave} />
          {/if}

          {#if activeTab === 'cli'}
            <SettingsTabCli
              bind:settings
              {t}
              {cliStatus}
              {cliInstalling}
              {cliStatusMessage}
              {cliStatusKind}
              {scheduleSave}
              {runCliInstall}
            />
          {/if}

          {#if activeTab === 'advanced'}
            <SettingsTabAdvanced
              bind:settings
              {t}
              {capabilities}
              {capabilityRows}
              {updateChecking}
              {updateStatus}
              {updateStatusKind}
              {updateReleaseUrl}
              {updateDownloadSupported}
              debounceNumberMs={DEBOUNCE_NUMBER_MS}
              {scheduleSave}
              {runUpdateCheck}
              {capabilityStatusLabel}
              {capabilityDetail}
              {showSetupButton}
              onOpenSetup={() => (activeTab = 'setup')}
            />
          {/if}
        </div>
      {/if}
    </form>
  {:else if !loading && !error}
    <p class="status hint">{t.settings.tauriRequired}</p>
  {/if}
</section>

<style>
  .settings {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 1rem;
    height: 100%;
    padding: 1.5rem;
    background: var(--bg, #14161a);
    color: var(--fg, #f5f5f5);
    overflow: auto;
  }
  /* On wide windows the form would otherwise stretch edge-to-edge,
     leaving selects and inputs awkwardly long. Cap the readable width
     and center the content so each row stays scannable. */
  .settings > .head,
  .settings > .tabs,
  .settings > .status,
  .settings > form {
    width: 100%;
    max-width: 52rem;
    margin-inline: auto;
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
  .head-trailing {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  /* Right-aligned status pill in the header. Reserving a min-width keeps
     the back-to-palette button locked in place when the label flickers
     between "Saving…" / "Saved" / hidden between edits. */
  .save-status {
    font-size: 0.75rem;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    min-width: 9rem;
    text-align: right;
  }
  .save-status[data-status='saved'] {
    color: #4ade80;
  }
  .save-status[data-status='error'] {
    color: var(--danger, #f87171);
  }
  .close {
    padding: 0.45rem 0.9rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 6px;
    background: transparent;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .close:hover {
    background: rgba(255, 255, 255, 0.06);
  }
  .status {
    color: var(--muted, rgba(255, 255, 255, 0.5));
  }
  .status.error {
    color: var(--danger, #f87171);
  }
  .tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  }
  /* Fieldsets are direct children of the form (Setup branch) or of the
     `.tab-content` wrapper (non-Setup tabs). Both layers use a column
     flex layout so the gap between CAPTURE / PALETTE DISPLAY / HOTKEYS / …
     applies regardless of which level renders the fieldsets. */
  form,
  .tab-content {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
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
  /* Shared form-control rules are hoisted to `:global(.settings …)` so
     they reach into the per-tab child components. Svelte scoped CSS does
     not traverse component boundaries, so without this every tab would
     have to re-declare its own copy of these rules. The selectors are
     still bounded by `.settings` so they cannot leak into the palette
     or other windows. */
  .tab-content :global(fieldset) {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 8px;
    padding: 0.75rem 1rem;
  }
  .tab-content :global(legend) {
    padding: 0 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .tab-content :global(label) {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.875rem;
  }
  .tab-content :global(label.stack),
  .tab-content :global(.stack) {
    flex-direction: column;
    align-items: stretch;
    gap: 0.35rem;
  }
  .tab-content :global(.stack) {
    display: flex;
    font-size: 0.875rem;
  }
  .tab-content :global(.help) {
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .tab-content :global(.hint) {
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .tab-content :global(.status) {
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .tab-content :global(.status.error) {
    color: var(--danger, #f87171);
  }
  .tab-content :global(.status.warning) {
    margin: 0;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--warning, #f59e0b);
    border-radius: 6px;
    background: rgba(245, 158, 11, 0.08);
    color: var(--warning, #f59e0b);
    font-size: 0.75rem;
    line-height: 1.4;
  }
  .tab-content :global(input[type='number']),
  .tab-content :global(textarea),
  .tab-content :global(select) {
    flex: 1;
    min-width: 0;
    padding: 0.45rem 0.6rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 6px;
    background: var(--bg-elevated, rgba(255, 255, 255, 0.04));
    color: inherit;
    font: inherit;
  }
  /* Cap fixed-width-feeling controls with max-width (not flex-basis) so
     they behave correctly inside both row and column flex containers —
     basis would otherwise be interpreted as height in `.stack` rows. */
  .tab-content :global(input[type='number']) {
    max-width: 9rem;
  }
  .tab-content :global(select) {
    max-width: 22rem;
    /* Native <select> popups can't composite a translucent background: on
       Windows/Linux the option list collapses to near-identical fg/bg and the
       labels disappear. Paint the control and its options with solid theme
       colors so the dropdown stays legible on every platform. */
    background: var(--bg-overlay, #1d1f23);
    color: var(--fg, #f5f5f5);
  }
  .tab-content :global(select option) {
    background: var(--bg-overlay, #1d1f23);
    color: var(--fg, #f5f5f5);
  }
  /* Labeled control rows: a fixed label column keeps the controls aligned
     across rows, while each control sizes to its own content and anchors
     to the start of the control column. Without this the shared
     `flex: 1` above stretches short selects (Theme, Default paste
     format, …) out to the full row width. Anchored here rather than in
     the child component so it outranks the `label { display: flex }` rule
     above (equal specificity would otherwise let flex win). */
  .tab-content :global(label.field-row) {
    display: grid;
    grid-template-columns: minmax(8rem, 12rem) 1fr;
    align-items: center;
    gap: 0.5rem;
  }
  .tab-content :global(label.field-row > input),
  .tab-content :global(label.field-row > select) {
    flex: none;
    width: auto;
    max-width: 100%;
    justify-self: start;
  }
  /* WKWebView desaturates native form controls when the window goes
     inactive, so the checked-state tint flickers between blue and gray
     each time focus returns. `accent-color` alone isn't enough on macOS
     — the renderer still applies the inactive overlay — so paint the
     box ourselves with `appearance: none`. The checkmark is an inline
     SVG drawn in `--bg` so it pops against the accent fill.
     CSP note: `img-src` already allows `data:`, so the inline SVG
     loads without a manifest change. */
  .tab-content :global(input[type='checkbox']) {
    appearance: none;
    -webkit-appearance: none;
    width: 1rem;
    height: 1rem;
    margin: 0;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.25));
    border-radius: 4px;
    background-color: var(--bg-elevated, rgba(255, 255, 255, 0.04));
    background-position: center;
    background-repeat: no-repeat;
    background-size: 78%;
    cursor: pointer;
    flex-shrink: 0;
    vertical-align: middle;
  }
  .tab-content :global(input[type='checkbox']:checked) {
    background-color: var(--accent, #6c8dff);
    border-color: var(--accent, #6c8dff);
    background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'><path fill='none' stroke='white' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round' d='M3.5 8.5l3.5 3.5L13 5.5'/></svg>");
  }
  .tab-content :global(input[type='checkbox']:focus-visible) {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  .tab-content :global(input[type='checkbox']:disabled) {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .tab-content :global(textarea) {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    resize: vertical;
  }
  .tab-content :global(.actions) {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    /* Pin the action row to the start of its row so the button doesn't
       stretch when the parent grows; flex column children default to
       `align-items: stretch`. */
    align-self: flex-start;
  }
  .tab-content :global(.actions button) {
    padding: 0.45rem 1.2rem;
    border: 1px solid transparent;
    border-radius: 6px;
    background: var(--accent, #6c8dff);
    /* On-accent foreground, not --bg: --accent stays blue in both themes, so
       --bg text (near-white in light) drops to ~2.9:1 on it. --accent-fg is
       #14161a in both themes — identical to --bg in dark, legible in light. */
    color: var(--accent-fg, #14161a);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  /* Lower-emphasis variant used by maintenance actions like "Check for
     update" — same footprint, but reads as a secondary control rather
     than competing with the primary action for attention. */
  .tab-content :global(.actions button.secondary) {
    background: transparent;
    color: inherit;
    border-color: var(--border, rgba(255, 255, 255, 0.18));
    font-weight: 500;
  }
  .tab-content :global(.actions button.compact) {
    padding: 0.25rem 0.7rem;
    font-size: 0.8rem;
  }
  .tab-content :global(.actions button:not(:disabled):hover) {
    filter: brightness(1.08);
  }
  .tab-content :global(.actions button.secondary:not(:disabled):hover) {
    background: rgba(255, 255, 255, 0.06);
    filter: none;
  }
  .tab-content :global(.actions button:disabled) {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>

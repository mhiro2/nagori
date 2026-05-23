<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { describeError } from "../lib/errors";
  import { checkForUpdates, getCapabilities, getSettings, updateSettings } from "../lib/commands";
  import { LOCALE_PREFERENCES, i18nState, messages, setLocale } from "../lib/i18n/index.svelte";
  import {
    MAX_USER_REGEX_LEN,
    MAX_USER_REGEX_NESTING,
    validateUserRegex,
    type UserRegexError,
  } from "../lib/policyValidation";
  import { TAURI_EVENTS, isTauri, subscribe } from "../lib/tauri";
  import { applyAppearance } from "../lib/theme";
  import {
    CONTENT_KINDS,
    type Appearance,
    type AppSettings,
    type Capability,
    type ContentKind,
    type LocaleSetting,
    type PaletteHotkeyAction,
    type PasteFormat,
    type PlatformCapabilities,
    type RecentOrder,
    type SecondaryHotkeyAction,
    type SecretHandling,
  } from "../lib/types";
  import { showPalette } from "../stores/view.svelte";

  type HotkeyFailurePayload = { hotkey: string; error: string };
  type SaveStatus = "idle" | "saving" | "saved" | "error";

  type Tab = "general" | "privacy" | "cli" | "advanced";

  const TABS: readonly Tab[] = ["general", "privacy", "cli", "advanced"];
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

  // Debounce profiles per control class. Checkbox / select edits commit in
  // a single discrete event, so 0 ms keeps the on-disk file in lock-step
  // with the toggle. Free-form text inputs fire `oninput` per keystroke,
  // so a window lets bursts coalesce into one `update_settings` call.
  const DEBOUNCE_NUMBER_MS = 350;
  const DEBOUNCE_TEXTAREA_MS = 500;
  // How long the "Saved" pill lingers after a successful round-trip
  // before the header collapses back to `idle`. Long enough to register
  // visually, short enough to stay out of the way of the next edit.
  const SAVED_HOLD_MS = 1500;
  // Cool-down between automatic retries after a failed save. The user
  // has no manual retry button (we deleted Save going macOS-style
  // silent-autosave), so without this a transient backend hiccup would
  // leave the snapshot stranded until the user edits again or closes
  // Settings. 5 s is long enough to absorb a brief IPC blip without
  // hammering, short enough to recover before the user wanders off.
  const RETRY_DELAY_MS = 5000;

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
  // Static OS capability matrix surfaced read-only in the Advanced tab.
  // Best-effort: failure to load is silently ignored — the section
  // hides rather than spamming the user with a non-actionable error.
  let capabilities: PlatformCapabilities | null = $state(null);
  let activeTab: Tab = $state("general");
  let loading = $state(false);
  let error: string | undefined = $state(undefined);
  let appDenylistText = $state("");
  let regexDenylistText = $state("");
  // `hydrated` flips true only after `get_settings` resolves *and* the
  // derived textarea state is in sync. Auto-save gates on this flag so
  // the initial render — which assigns `settings`, `appDenylistText`,
  // and `regexDenylistText` in sequence — cannot accidentally feed the
  // defaults straight back to disk.
  let hydrated = $state(false);
  let saveStatus = $state<SaveStatus>("idle");
  let saveError: string | undefined = $state(undefined);
  // Single shared debounce timer — any new edit cancels the pending
  // tick so we coalesce bursts into one `update_settings` round-trip.
  let pendingTimer: ReturnType<typeof setTimeout> | null = null;
  // At most one in-flight save; concurrent edits set `queued` and the
  // post-commit hook drains it. Full-snapshot semantics mean last-write
  // wins, so a single follow-up flag replaces a proper queue.
  let inflight: Promise<void> | null = null;
  let queued = false;
  // Raised by `applyRemoteSettings` when an external `settings_changed`
  // event lands while a save is in flight. The commitSave success/catch
  // branches use this to skip writing back `lastPersistedJson` (the merge
  // already advanced it to the remote snapshot), and the finally hook
  // fires a follow-up commit so the merged local state — which may now
  // diverge from the snapshot the backend just accepted — actually
  // reaches disk. Without this, a tray toggle that races against an
  // in-flight save can be silently overwritten when the success branch
  // restores `lastPersistedJson` to the pre-merge snapshot.
  let externalMergeDuringInflight = false;
  let savedTimer: ReturnType<typeof setTimeout> | null = null;
  // Set in the `updateSettings` failure branch and cleared on success
  // or unmount. Fires `commitSave` again so the snapshot keeps trying
  // even when the user makes no further edits — the only other retry
  // triggers are a new edit or the unmount flush, neither of which
  // covers "user opened Settings, save failed, user does nothing".
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  // The exact JSON payload that failed, captured at the moment of
  // failure. The retry re-sends this verbatim instead of re-reading
  // live state — `bind:value` on the hotkey textbox updates
  // `settings.globalHotkey` on every keystroke (the save trigger is
  // `onblur`), so building a fresh snapshot at retry-fire time would
  // leak a half-typed accelerator like "Cmd+Sh…" into the IPC and
  // defeat the "no partial hotkeys" design.
  let pendingRetryJson: string | null = null;
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
  let lastBlurredGlobalHotkey = "";
  let lastBlurredPaletteHotkeys: Partial<Record<PaletteHotkeyAction, string>> = {};
  let lastBlurredSecondaryHotkeys: Partial<Record<SecondaryHotkeyAction, string>> = {};
  // JSON-serialised form of the last payload we handed to
  // `updateSettings`, set *before* the IPC is dispatched. Used by
  // `commitSave` to suppress idempotent IPC — typing in a broken regex
  // textarea keeps re-emitting the previous valid list, so without an
  // equality short-circuit the header pill flashes for nothing. Also
  // covers the "rapid undo" case: if the user toggles a checkbox while
  // an earlier in-flight save is still settling, the next snapshot may
  // equal the *pre-inflight* state, so comparing only against the
  // persisted baseline would skip the corrective write the in-flight
  // save's resolution will need.
  let lastSentJson = "";
  // JSON-serialised form of the last payload the backend acknowledged.
  // Advances only inside the success branch of `updateSettings`, never
  // optimistically. The failure branch rewinds `lastSentJson` to this
  // value so the cool-down retry / unmount flush can re-send the
  // payload the backend rejected — without that the dedup
  // short-circuit at the top of `commitSave` would silently drop the
  // retry.
  let lastPersistedJson = "";
  // Live preflight against the same limits `compile_user_regex` enforces in
  // `nagori-core::policy`. Rendered inline next to the textarea so the user
  // sees per-line guidance ("too long", "nested too deep", "invalid syntax")
  // before the daemon would otherwise reject the save. The validator's
  // `index` is set to the textarea's 1-based row number minus one so the
  // rendered `Line N` label matches the row the user is editing, even when
  // blank lines sit between entries.
  let regexDenylistErrors = $derived.by<UserRegexError[]>(() =>
    regexDenylistText
      .split(/\r?\n/)
      .flatMap((line, idx) => {
        const trimmed = line.trim();
        if (trimmed.length === 0) return [];
        const err = validateUserRegex(trimmed, idx);
        return err ? [err] : [];
      }),
  );
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

  let updateDownloadSupported = $state(true);

  const runUpdateCheck = async (): Promise<void> => {
    if (updateChecking) return;
    updateChecking = true;
    updateStatus = undefined;
    updateReleaseUrl = undefined;
    try {
      const info = await checkForUpdates();
      updateStatusKind = "info";
      if (info) {
        updateDownloadSupported = info.downloadSupported;
        // Whether the install medium supports in-place replacement
        // decides the wording: AppImage/NSIS/.app can swap the bundle
        // automatically, a `.deb` install needs the user to fetch a
        // new package manually. We always link to the GitHub release
        // page; the difference is the surrounding copy.
        updateStatus = info.downloadSupported
          ? t.settings.updates.available.replace("{version}", info.version)
          : t.settings.updates.availableManual.replace("{version}", info.version);
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
        lastValidRegexList = [...s.regexDenylist];
        lastBlurredGlobalHotkey = s.globalHotkey;
        lastBlurredPaletteHotkeys = { ...s.paletteHotkeys };
        lastBlurredSecondaryHotkeys = { ...s.secondaryHotkeys };
        setLocale(s.locale);
        applyAppearance(s.appearance);
        // All form-bound state is now in sync with the backend snapshot;
        // arming `hydrated` here means handlers fired during the initial
        // bindings (e.g. Svelte's two-way binding pass) cannot trigger
        // a spurious save.
        hydrated = true;
        // The freshly-loaded form already matches what's on disk, so
        // seed both baselines from the same snapshot. This suppresses a
        // no-op save on the first commit after hydration and keeps the
        // unmount flush quiet when the user only opened Settings to
        // read.
        const initialJson = JSON.stringify(buildSnapshotPayload());
        lastSentJson = initialJson;
        lastPersistedJson = initialJson;
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
  });

  type CapabilityRow = {
    label: string;
    capability: Capability;
  };

  const capabilityRows = $derived.by<CapabilityRow[]>(() => {
    if (!capabilities) return [];
    return [
      { label: "Capture text", capability: capabilities.captureText },
      { label: "Capture image", capability: capabilities.captureImage },
      { label: "Capture files", capability: capabilities.captureFiles },
      { label: "Write text", capability: capabilities.writeText },
      { label: "Write image", capability: capabilities.writeImage },
      {
        label: "Multi-representation copy-back",
        capability: capabilities.clipboardMultiRepresentationWrite,
      },
      { label: "Auto-paste", capability: capabilities.autoPaste },
      { label: "Global hotkey", capability: capabilities.globalHotkey },
      { label: "Frontmost app", capability: capabilities.frontmostApp },
      { label: "Permissions UI", capability: capabilities.permissionsUi },
      { label: "Update check", capability: capabilities.updateCheck },
      { label: "Preview (Quick Look)", capability: capabilities.previewQuickLook },
    ];
  });

  const capabilityStatusLabel = (capability: Capability): string => {
    switch (capability.status) {
      case "available":
        return "Available";
      case "unsupported":
        return "Unsupported";
      case "requiresPermission":
        return "Permission";
      case "requiresExternalTool":
        return "External tool";
      case "experimental":
        return "Experimental";
    }
  };

  const capabilityDetail = (capability: Capability): string => {
    switch (capability.status) {
      case "available":
        return "";
      case "unsupported":
        return capability.reason;
      case "requiresPermission":
        return `${capability.permission} — ${capability.message}`;
      case "requiresExternalTool":
        return capability.installHint
          ? `${capability.tool} (${capability.installHint})`
          : capability.tool;
      case "experimental":
        return capability.message;
    }
  };

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
    appDenylist: linesToList(appDenylistText),
    regexDenylist:
      regexDenylistErrors.length === 0
        ? linesToList(regexDenylistText)
        : lastValidRegexList,
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

  const clearRetryTimer = (): void => {
    if (retryTimer !== null) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
    pendingRetryJson = null;
  };

  // Send the captured retry payload, deferring if a save is already in
  // flight. Riding the `queued` drain instead would lose the override
  // — the drain calls `commitSave()` with no argument, which rebuilds
  // the snapshot from live state and could leak a mid-typed hotkey
  // accelerator. Chaining off `inflight.finally` keeps the retry
  // payload pinned across an arbitrarily long save chain.
  const fireRetry = (): void => {
    if (destroyed) return;
    const payload = pendingRetryJson;
    if (payload === null) return;
    if (inflight) {
      // `.finally` callbacks run in registration order: the original
      // `await inflight` continuation (the outer `finally` that
      // potentially starts a drain) fires first, so by the time our
      // handler runs the next `inflight` is either set (drain
      // started) or null. Re-evaluate from scratch.
      void inflight.finally(() => {
        fireRetry();
      });
      return;
    }
    void commitSave(payload);
  };

  const scheduleSave = (delay: number): void => {
    if (!hydrated || !settings || destroyed) return;
    // A fresh user edit supersedes any cooled-down auto-retry. The edit
    // path will call `commitSave` anyway, so leaving the retry armed
    // would just produce a duplicate IPC moments later.
    clearRetryTimer();
    if (pendingTimer !== null) {
      clearTimeout(pendingTimer);
      pendingTimer = null;
    }
    if (delay === 0) {
      void commitSave();
      return;
    }
    pendingTimer = setTimeout(() => {
      pendingTimer = null;
      void commitSave();
    }, delay);
  };

  // `overrideJson` is supplied by the retry timer to re-submit the
  // exact payload that failed earlier, bypassing the live-state read
  // in `buildSnapshotPayload`. Without it the retry would pick up a
  // mid-typed hotkey accelerator from the textbox's two-way binding.
  const commitSave = async (overrideJson?: string): Promise<void> => {
    if (!hydrated || !settings || destroyed) return;

    if (inflight) {
      // Full-snapshot semantics give us last-write-wins — a single
      // follow-up flag replaces a proper queue. The post-commit hook
      // re-invokes once and the latest snapshot wins.
      queued = true;
      return;
    }

    // Skip a backend round-trip when the payload matches what we just
    // sent. The JSON round-trip also detaches the IPC payload from the
    // live `$state` proxy so a follow-up edit while `updateSettings` is
    // in flight can't mutate the snapshot mid-call; `structuredClone`
    // is unsuitable because jsdom (vitest) refuses to clone Svelte's
    // reactive Array proxy.
    const snapshotJson = overrideJson ?? JSON.stringify(buildSnapshotPayload());
    if (snapshotJson === lastSentJson) return;
    const snapshot = JSON.parse(snapshotJson) as AppSettings;
    // Record the send *before* awaiting so a follow-up commit during
    // the in-flight window can short-circuit if it ends up emitting the
    // same payload.
    lastSentJson = snapshotJson;

    saveStatus = "saving";
    if (savedTimer !== null) {
      clearTimeout(savedTimer);
      savedTimer = null;
    }

    inflight = (async () => {
      externalMergeDuringInflight = false;
      try {
        await updateSettings(snapshot);
        // Advance the persisted baseline before the destroyed check so
        // a successful save that lands during teardown still updates the
        // record the unmount flush would otherwise re-send. Skip when an
        // external merge happened mid-flight — `applyRemoteSettings`
        // already advanced `lastPersistedJson` to the merged remote
        // snapshot, and clobbering it with the pre-merge `snapshotJson`
        // here would let the next echo silently revert the merge.
        if (!externalMergeDuringInflight) {
          lastPersistedJson = snapshotJson;
        }
        if (destroyed) return;
        // If another edit was already queued while we were in flight
        // skip the "Saved" pill — the next commit will flip the header
        // back to "Saving…" within the same tick anyway.
        if (!queued) {
          saveStatus = "saved";
          saveError = undefined;
          error = undefined;
          savedTimer = setTimeout(() => {
            savedTimer = null;
            if (saveStatus === "saved") saveStatus = "idle";
          }, SAVED_HOLD_MS);
        }
        // A retry timer left over from an earlier failure is now moot —
        // the most recent snapshot has landed.
        clearRetryTimer();
      } catch (err: unknown) {
        // Leave `lastPersistedJson` untouched and rewind `lastSentJson`
        // to it. The cool-down retry below and the unmount flush both
        // rebuild a snapshot from live state and compare against
        // `lastSentJson`; without the rewind the dedup short-circuit
        // at the top of `commitSave` would silently skip the retry of
        // the exact same payload.
        //
        // Apply the rewind unconditionally — including when an external
        // merge happened mid-flight. In that case `lastPersistedJson`
        // is the merged remote snapshot R; aligning `lastSentJson` to R
        // lets the follow-up commit in `finally` dispatch whenever the
        // merged live snapshot still diverges from R (the common case
        // when the user has preserved-dirty fields), and dedup only
        // when it doesn't. Skipping the rewind here used to leave
        // `lastSentJson` at the failed pre-merge dispatch L; if the
        // merge net-cancelled the in-flight edits the follow-up
        // snapshot would equal L and the dedup check would silently
        // drop the user's intent.
        lastSentJson = lastPersistedJson;
        if (destroyed) return;
        saveStatus = "error";
        saveError = describeError(err);
        if (externalMergeDuringInflight) {
          // The follow-up commit triggered from `finally` will dispatch
          // the merged state; if it also fails its own catch branch
          // will arm a fresh retry. Re-arming the timer here would
          // double-fire and risks the pre-merge snapshot landing after
          // the merge follow-up.
          clearRetryTimer();
        } else {
          // Re-fire the save after a brief cool-down. Without this the
          // failed snapshot would be stranded until the user either
          // edits again or closes Settings — a transient IPC blip would
          // appear as a permanent error pill. Each retry that also fails
          // hits this branch again and reschedules, giving an indefinite
          // 5 s poll until the backend recovers or the user navigates
          // away. We accept the trade-off of polling against a
          // permanently broken backend for the simpler state machine.
          // Capture the snapshot the backend just rejected and feed it
          // back into `commitSave` verbatim — re-reading live state in
          // the timer callback would pull in any mid-typed hotkey value
          // the user has tapped out since the failure landed.
          clearRetryTimer();
          pendingRetryJson = snapshotJson;
          retryTimer = setTimeout(() => {
            retryTimer = null;
            fireRetry();
          }, RETRY_DELAY_MS);
        }
      }
    })();

    try {
      await inflight;
    } finally {
      inflight = null;
      // Drain order: a queued local edit and a pending external-merge
      // follow-up both want to fire a fresh commit. Either one alone is
      // enough — `commitSave` will rebuild from the current settings,
      // which already reflects both signals. So OR them into a single
      // dispatch instead of firing twice and chasing our own tail.
      const needsExternalMergeFollowUp = externalMergeDuringInflight;
      externalMergeDuringInflight = false;
      if ((queued || needsExternalMergeFollowUp) && !destroyed) {
        queued = false;
        void commitSave();
      }
    }
  };

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
    switch (saveStatus) {
      case "saving":
        return t.settings.statusSaving;
      case "saved":
        return t.settings.statusSaved;
      case "error":
        return t.settings.statusError.replace("{error}", saveError ?? "");
      case "idle":
        return "";
    }
  });

  let destroyed = false;

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
    if (typeof a !== "object" || typeof b !== "object") return false;
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
    if (!hydrated || destroyed || !settings) return;
    const remoteJson = JSON.stringify(remote);
    // Echo of our own most-recent dispatch — the backend has confirmed
    // the write landed. Advance the persisted baseline so subsequent
    // remote events evaluate against reality, but leave local state
    // alone: anything the user has typed since we sent the snapshot
    // stays as an unflushed edit.
    if (remoteJson === lastSentJson) {
      // While `externalMergeDuringInflight` is set, an external merge
      // has already advanced `lastPersistedJson` to the remote snapshot;
      // an echo of our pre-merge dispatch that lands afterwards would
      // otherwise rewind the baseline to the stale snapshot and let a
      // follow-up remote event be misclassified as dirty. Leave the
      // baseline alone here — the follow-up commit will re-sync it.
      if (!externalMergeDuringInflight) {
        lastPersistedJson = remoteJson;
      }
      return;
    }
    const baseline = JSON.parse(lastPersistedJson) as AppSettings;
    const local = settings;
    // Denylist fields live in textarea state (`appDenylistText` /
    // `regexDenylistText`), not in `settings` directly — `settings`
    // only catches up when `buildSnapshotPayload` runs at save time.
    // Compare the *raw textarea text* against the baseline's stringified
    // form for the dirty check. Using `linesToList` for the comparison
    // would drop trailing newlines a user has typed but not yet finished
    // a line on, and for the regex case substituting `lastValidRegexList`
    // when the textarea is invalid would falsely classify a mid-typed
    // broken regex as clean and let `remote` overwrite the user's input.
    const baselineAppText = baseline.appDenylist.join("\n");
    const baselineRegexText = baseline.regexDenylist.join("\n");
    const appDenylistDirty = appDenylistText !== baselineAppText;
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
        (local as unknown as Record<string, unknown>)[key] =
          remote[key] as unknown;
      }
    }
    // Re-derive UI state for adopted fields. Reuse the dirty flags from
    // the loop above so a textarea we just classified as user-edited
    // keeps its in-progress content (and `lastValidRegexList`) intact.
    if (!appDenylistDirty) {
      appDenylistText = remote.appDenylist.join("\n");
    }
    if (!regexDenylistDirty) {
      regexDenylistText = remote.regexDenylist.join("\n");
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

    // Backend is now authoritative at `remote`. Advance the merge
    // baseline so the next event re-evaluates against the latest
    // persisted state. When no save is in flight we can also realign
    // `lastSentJson` to `remoteJson` — the dedup check at the top of
    // `commitSave` then short-circuits when the user has no unflushed
    // edits, but a divergent next snapshot (e.g. a debounce-pending
    // number input that the merge preserved) still dispatches.
    //
    // Using `remoteJson` instead of `buildSnapshotPayload()` is
    // load-bearing: the latter folds preserved-dirty fields into the
    // baseline, so the next commit would dedup against state that has
    // not actually been sent and the user's edit would be silently
    // dropped.
    //
    // Skip the realignment while a save is in flight: the in-flight
    // dispatch is still the source of truth for `lastSentJson`, and
    // the `finally` hook will fire a follow-up commit (via
    // `externalMergeDuringInflight`) that re-syncs both pointers.
    lastPersistedJson = remoteJson;
    if (inflight === null) {
      lastSentJson = remoteJson;
    } else {
      externalMergeDuringInflight = true;
    }

    // A retry timer armed against a pre-merge failure would re-send the
    // stale snapshot and silently undo the external mutation. Cancel it
    // and immediately schedule a fresh commit from the merged live state
    // — if the user's preserved-dirty fields still diverge from `remote`
    // the new dispatch ships them, and otherwise the dedup check
    // short-circuits without an IPC. Skip when a save is already in
    // flight: the `finally` hook will fire the follow-up commit via
    // `externalMergeDuringInflight` and we do not want to double-dispatch.
    if (pendingRetryJson !== null) {
      clearRetryTimer();
      if (inflight === null) {
        scheduleSave(0);
      }
    }
  };

  onMount(() => {
    const offHotkey = subscribe<HotkeyFailurePayload>(
      TAURI_EVENTS.hotkeyRegisterFailed,
      (payload) => {
        hotkeyError = payload.error || payload.hotkey;
      },
    );
    const offSettings = subscribe<AppSettings>(
      TAURI_EVENTS.settingsChanged,
      applyRemoteSettings,
    );
    return () => {
      offHotkey();
      offSettings();
    };
  });

  onDestroy(() => {
    if (pendingTimer !== null) {
      clearTimeout(pendingTimer);
      pendingTimer = null;
    }
    if (savedTimer !== null) {
      clearTimeout(savedTimer);
      savedTimer = null;
    }
    clearRetryTimer();
    // Flush any in-memory edits the user hasn't given the debounce / blur
    // path a chance to commit. Covers three loss paths: a textarea or
    // number input still inside its debounce window when the user
    // navigates away, a queued snapshot waiting on the post-commit drain
    // (won't fire once `destroyed` is set), and a hotkey field that was
    // edited via `setOverride` but never blurred (Escape -> palette
    // tears the focused input off the DOM without firing `blur`). The
    // webview context outlives the Svelte component, so a fire-and-forget
    // `updateSettings` reaches Tauri even though the UI is already gone.
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
      // Swallow the error: the component is unmounting, so the status
      // pill is already gone and there's no surface left to render a
      // failure on. The next session reloads from disk anyway.
      const dispatchFinal = (): void => {
        void updateSettings(snapshot).catch(() => {});
      };
      if (inflight) {
        // Defer the decision until the in-flight save settles. Comparing
        // the live snapshot to `lastPersistedJson` at settle-time covers
        // every interleaving:
        //   • in-flight succeeds at the same payload → snapshot ==
        //     lastPersistedJson, skip (no duplicate IPC),
        //   • in-flight succeeds but the user reverted / followed up so
        //     the queued drain would have fired (gated off by
        //     `destroyed`) → snapshot != lastPersistedJson, dispatch,
        //   • in-flight fails entirely → its catch rewinds
        //     `lastSentJson` and bails on `destroyed` before arming
        //     the retry timer, leaving `lastPersistedJson` at the
        //     pre-edit baseline → snapshot != lastPersistedJson,
        //     dispatch (the only path left for the edit to survive).
        // Chaining off `.finally` instead of firing in parallel
        // serialises against the in-flight save: the backend's SQLite
        // pool uses multiple connections, so two parallel
        // `update_settings` could settle out of order.
        void inflight.finally(() => {
          if (snapshotJson !== lastPersistedJson) dispatchFinal();
        });
      } else if (snapshotJson !== lastSentJson) {
        // No in-flight: an earlier failure may have rewound
        // `lastSentJson` to the persisted baseline (and we just
        // cleared its retry timer above), or this is the first save
        // attempt. Either way, snapshot ≠ lastSentJson means there's
        // an unflushed edit that needs to land.
        dispatchFinal();
      }
    }
    destroyed = true;
  });
</script>

<section class="settings">
  <header class="head">
    <h1>{t.settings.title}</h1>
    <div class="head-trailing">
      {#if saveStatus !== "idle"}
        <span class="save-status" data-status={saveStatus} aria-live="polite">
          {saveStatusLabel}
        </span>
      {/if}
      <button type="button" class="close" onclick={showPalette}>
        {t.settings.backToPalette}
      </button>
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

    <form onsubmit={(e) => e.preventDefault()}>
    {#if activeTab === "general"}
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
        <label>
          {t.settings.capture.pasteFormatDefault}
          <select
            bind:value={settings.pasteFormatDefault}
            onchange={(e) => {
              if (!settings) return;
              settings.pasteFormatDefault = (e.target as HTMLSelectElement).value as PasteFormat;
              scheduleSave(0);
            }}
          >
            <option value="preserve">{t.settings.capture.pasteFormatOptions.preserve}</option>
            <option value="plain_text">{t.settings.capture.pasteFormatOptions.plain_text}</option>
          </select>
        </label>
        <label>
          {t.settings.capture.hotkey}
          <input
            type="text"
            bind:value={settings.globalHotkey}
            onblur={() => {
              if (!settings) return;
              lastBlurredGlobalHotkey = settings.globalHotkey;
              scheduleSave(0);
            }}
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
              scheduleSave(DEBOUNCE_NUMBER_MS);
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
                onblur={() => {
                  if (!settings) return;
                  lastBlurredPaletteHotkeys = { ...settings.paletteHotkeys };
                  scheduleSave(0);
                }}
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
                onblur={() => {
                  if (!settings) return;
                  lastBlurredSecondaryHotkeys = { ...settings.secondaryHotkeys };
                  scheduleSave(0);
                }}
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
              scheduleSave(0);
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
          <input
            type="checkbox"
            bind:checked={settings.autoLaunch}
            onchange={() => scheduleSave(0)}
          />
          {t.settings.integration.autoLaunch}
        </label>
        <p class="help">{t.settings.integration.autoLaunchHelp}</p>
        <label>
          <input
            type="checkbox"
            bind:checked={settings.showInMenuBar}
            onchange={() => scheduleSave(0)}
          />
          {t.settings.integration.menuBar}
        </label>
        <p class="help">{t.settings.integration.menuBarHelp}</p>
        <label>
          <input
            type="checkbox"
            bind:checked={settings.clearOnQuit}
            onchange={() => scheduleSave(0)}
          />
          {t.settings.integration.clearOnQuit}
        </label>
        <p class="help">{t.settings.integration.clearOnQuitHelp}</p>
      </fieldset>
    {/if}

    {#if activeTab === "privacy"}
      <fieldset>
        <legend>{t.settings.privacy.legend}</legend>
        <label class="stack">
          {t.settings.privacy.appDenylist}
          <textarea
            rows="4"
            bind:value={appDenylistText}
            oninput={() => scheduleSave(DEBOUNCE_TEXTAREA_MS)}
          ></textarea>
          <span class="help">{t.settings.privacy.appDenylistHelp}</span>
        </label>
        <label class="stack">
          {t.settings.privacy.regexDenylist}
          <textarea
            rows="4"
            bind:value={regexDenylistText}
            oninput={() => scheduleSave(DEBOUNCE_TEXTAREA_MS)}
            aria-invalid={regexDenylistErrors.length > 0 || undefined}
            aria-describedby={regexDenylistErrors.length > 0
              ? "regex-denylist-help regex-denylist-errors regex-denylist-autosave"
              : "regex-denylist-help regex-denylist-autosave"}
          ></textarea>
          <span class="help" id="regex-denylist-help">
            {t.settings.privacy.regexDenylistHelp}
          </span>
          {#if regexDenylistErrors.length > 0}
            <ul id="regex-denylist-errors" class="status error regex-errors" role="alert">
              {#each regexDenylistErrors as err (`${err.index}:${err.kind}`)}
                <li>
                  <strong>
                    {t.settings.privacy.regexErrors.lineLabel.replace(
                      '{line}',
                      String(err.index + 1),
                    )}
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
              scheduleSave(0);
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
            oninput={() => scheduleSave(DEBOUNCE_NUMBER_MS)}
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
              scheduleSave(DEBOUNCE_NUMBER_MS);
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
              scheduleSave(DEBOUNCE_NUMBER_MS);
            }}
          />
          <span class="help">{t.settings.retention.maxTotalBytesHelp}</span>
        </label>
      </fieldset>
    {/if}

    {#if activeTab === "cli"}
      <fieldset>
        <legend>{t.settings.cli.legend}</legend>
        <label>
          <input
            type="checkbox"
            bind:checked={settings.cliIpcEnabled}
            onchange={() => scheduleSave(0)}
          />
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
            oninput={() => scheduleSave(DEBOUNCE_NUMBER_MS)}
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
            oninput={() => scheduleSave(DEBOUNCE_NUMBER_MS)}
          />
        </label>
      </fieldset>
      {#if capabilities}
        <fieldset>
          <legend>Platform capabilities</legend>
          <p class="help">
            What this OS build can do, independent of the live permission state.
            Use the Privacy/Permissions section above to grant access for items
            marked “Permission”.
          </p>
          <div class="capability-meta">
            <span><strong>Platform:</strong> {capabilities.platform}</span>
            <span><strong>Tier:</strong> {capabilities.tier}</span>
          </div>
          <table class="capability-table">
            <thead>
              <tr>
                <th scope="col">Capability</th>
                <th scope="col">Status</th>
                <th scope="col">Detail</th>
              </tr>
            </thead>
            <tbody>
              {#each capabilityRows as row (row.label)}
                <tr>
                  <th scope="row" class="capability-label">{row.label}</th>
                  <td>
                    <span
                      class="capability-status"
                      data-status={row.capability.status}
                    >
                      {capabilityStatusLabel(row.capability)}
                    </span>
                  </td>
                  <td class="capability-detail">{capabilityDetail(row.capability)}</td>
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
        <p class="help">{t.settings.updates.autoCheckHelp}</p>
        <p class="help">
          {t.settings.updates.channel}: <strong>{settings.updateChannel}</strong>
        </p>
        <div class="actions">
          <button
            type="button"
            class="secondary"
            disabled={updateChecking}
            onclick={runUpdateCheck}
          >
            {updateChecking ? t.settings.updates.checking : t.settings.updates.checkNow}
          </button>
        </div>
        {#if updateStatus}
          <p class="status" class:error={updateStatusKind === "error"}>
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
  .save-status[data-status="saved"] {
    color: #4ade80;
  }
  .save-status[data-status="error"] {
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
  .tabs {
    display: flex;
    gap: 0.25rem;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  }
  /* Fieldsets are direct children of the form; without an explicit gap they
     stack flush against each other. A column flex layout adds vertical
     breathing room between CAPTURE / PALETTE DISPLAY / HOTKEYS / … */
  form {
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
  input[type="number"] {
    max-width: 9rem;
  }
  select {
    max-width: 22rem;
  }
  /* WKWebView desaturates native form controls when the window goes
     inactive, so the checked-state tint flickers between blue and gray
     each time focus returns. `accent-color` alone isn't enough on macOS
     — the renderer still applies the inactive overlay — so paint the
     box ourselves with `appearance: none`. The checkmark is an inline
     SVG drawn in `--bg` so it pops against the accent fill.
     CSP note: `img-src` already allows `data:`, so the inline SVG
     loads without a manifest change. */
  input[type="checkbox"] {
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
  input[type="checkbox"]:checked {
    background-color: var(--accent, #6c8dff);
    border-color: var(--accent, #6c8dff);
    background-image: url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'><path fill='none' stroke='white' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round' d='M3.5 8.5l3.5 3.5L13 5.5'/></svg>");
  }
  input[type="checkbox"]:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  input[type="checkbox"]:disabled {
    opacity: 0.55;
    cursor: not-allowed;
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
    /* Pin the action row to the start of its row so the button doesn't
       stretch when the parent grows; flex column children default to
       `align-items: stretch`. */
    align-self: flex-start;
  }
  .actions button {
    padding: 0.45rem 1.2rem;
    border: 1px solid transparent;
    border-radius: 6px;
    background: var(--accent, #6c8dff);
    color: var(--bg, #14161a);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  /* Lower-emphasis variant used by maintenance actions like "Check for
     update" — same footprint, but reads as a secondary control rather
     than competing with the primary action for attention. */
  .actions button.secondary {
    background: transparent;
    color: inherit;
    border-color: var(--border, rgba(255, 255, 255, 0.18));
    font-weight: 500;
  }
  .actions button:not(:disabled):hover {
    filter: brightness(1.08);
  }
  .actions button.secondary:not(:disabled):hover {
    background: rgba(255, 255, 255, 0.06);
    filter: none;
  }
  .actions button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .hint {
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
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
  }
  .capability-table th,
  .capability-table td {
    padding: 0.25rem 0.6rem 0.25rem 0;
    text-align: left;
    font-weight: normal;
    vertical-align: baseline;
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
    justify-self: start;
    padding: 0.1rem 0.5rem;
    border-radius: 999px;
    font-size: 0.6875rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    background: rgba(255, 255, 255, 0.04);
    color: var(--muted, rgba(255, 255, 255, 0.7));
  }
  .capability-status[data-status="available"] {
    color: #4ade80;
    border-color: rgba(74, 222, 128, 0.4);
    background: rgba(74, 222, 128, 0.08);
  }
  .capability-status[data-status="unsupported"] {
    color: var(--danger, #f87171);
    border-color: rgba(248, 113, 113, 0.4);
    background: rgba(248, 113, 113, 0.08);
  }
  .capability-status[data-status="requiresPermission"],
  .capability-status[data-status="requiresExternalTool"],
  .capability-status[data-status="experimental"] {
    color: var(--warning, #f59e0b);
    border-color: rgba(245, 158, 11, 0.4);
    background: rgba(245, 158, 11, 0.08);
  }
  .capability-detail {
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.75rem;
  }
</style>

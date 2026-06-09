<script lang="ts">
  import { untrack } from 'svelte';

  import {
    cancelAiAction,
    getAiAvailability,
    runQuickAction,
    saveAiResult,
    startAiAction,
  } from '../lib/commands';
  import { describeError } from '../lib/errors';
  import { messages } from '../lib/i18n/index.svelte';
  import { isTauri, subscribe, TAURI_EVENTS } from '../lib/tauri';
  import type {
    AiAvailability,
    AiDeltaEvent,
    AiDoneEvent,
    AiErrorEvent,
    AiReplaceEvent,
    ContentKind,
    QuickActionId,
    SearchResultDto,
  } from '../lib/types';
  import { aiActionsSupported } from '../stores/capabilities.svelte';
  import ActionPicker from './ActionPicker.svelte';
  import ActionRunPanel from './ActionRunPanel.svelte';
  import CompactPreview from './CompactPreview.svelte';

  type Props = {
    target: SearchResultDto | undefined;
    open: boolean;
    onClose: () => void;
    // Recognises the `open-actions` chord (Cmd/Ctrl+K, or the user's remap).
    // The panel swallows its own keydowns while focused, so it needs this to
    // toggle itself closed on the same chord that opened it. Optional so the
    // component still renders in isolation (tests/non-Tauri) without it.
    matchesToggle?: (event: KeyboardEvent) => boolean;
  };

  const { target, open, onClose, matchesToggle }: Props = $props();

  // Deterministic, on-device transforms. They run synchronously through the
  // daemon and never touch a language model.
  const QUICK_ACTION_IDS: readonly QuickActionId[] = [
    'SummarizeFirstSentence',
    'FormatJson',
    'ExtractTasks',
    'RedactSecrets',
  ];

  // The streaming, model-backed text actions, in capability-matrix order.
  // `Translate` is omitted: it needs a target-language picker and is CLI-only
  // for now.
  type AiTextActionId = 'Summarize' | 'Rewrite' | 'FormatMarkdown' | 'ExtractTasks' | 'ExplainCode';
  const AI_ACTION_IDS: readonly AiTextActionId[] = [
    'Summarize',
    'Rewrite',
    'FormatMarkdown',
    'ExtractTasks',
    'ExplainCode',
  ];

  // Deterministic actions still pass through `running` internally, but the
  // running UI only appears once a run outlives this threshold — sub-150ms
  // transforms then read as idle→result (or result→result) with no flash.
  const QUICK_RUNNING_DELAY_MS = 120;
  // How long the copy/save "OK" and the post-completion "Done" flashes linger.
  const FLASH_MS = 1500;
  const DONE_FLASH_MS = 1200;

  const t = $derived(messages());

  let pending: QuickActionId | undefined = $state(undefined);
  let lastResult: string | undefined = $state(undefined);
  let runError: string | undefined = $state(undefined);
  let copyOk = $state(false);
  let saveOk = $state(false);
  let saving = $state(false);
  let panelEl: HTMLElement | undefined = $state();

  let quickRunningVisible = $state(false);
  let quickRunningTimer: ReturnType<typeof setTimeout> | undefined;

  // Streaming AI state. `aiRequestId` scopes the `nagori://ai/*` events we
  // accept; `aiText` is the request-local display buffer.
  let availability = $state<AiAvailability | undefined>(undefined);
  let aiRequestId = $state<string | undefined>(undefined);
  let aiText = $state('');
  let aiStreaming = $state(false);
  // Which AI action is currently streaming, for the inline spinner.
  let aiPendingAction = $state<AiTextActionId | undefined>(undefined);
  // Brief "Done" flash when an AI run completes, mirroring copy/save feedback.
  let doneFlash = $state(false);
  let doneFlashTimer: ReturnType<typeof setTimeout> | undefined;

  // Non-reactive guards. `runToken` fences a quick action's async result so a
  // run that resolves after the inspector was closed (and possibly reopened on
  // a different target) can't commit stale output. `cancelRequested` remembers a
  // cancel pressed during the AI startup window — before `startAiAction` has
  // returned a request id — so we can cancel the moment that id arrives.
  let runToken = 0;
  let cancelRequested = false;
  // Resolves once the request-scoped `nagori://ai/*` listeners have actually
  // attached on the backend (Tauri's `listen()` is async, so `subscribe()`
  // returning does not mean we are listening yet). `runAiAction` awaits this
  // before starting a run so a fast `done`/`error` can't be emitted in the gap
  // between subscribe and attach — which would drop the terminal event and
  // strand the UI in the running state. Reset to a fresh pending promise each
  // time the inspector (re)subscribes; resolved by default while unsubscribed.
  // `aiListenersAttached` is the synchronous fast-path: once true, a run skips
  // the await entirely, so the common open-then-click case adds no latency and
  // the gate only ever delays a click that lands inside the attach window.
  let aiListenersReady: Promise<void> = Promise.resolve();
  let aiListenersAttached = true;

  type FlashTimer = ReturnType<typeof setTimeout> | undefined;
  let copyFlashTimer: FlashTimer = undefined;
  let saveFlashTimer: FlashTimer = undefined;

  // A run is in flight whenever a quick action or an AI stream is active; the
  // whole picker disables so a second action can't race the first. The picker
  // stays visible (compact) so the user can chain runs once one settles.
  const busy = $derived(aiStreaming || pending !== undefined);

  // The single work-area state machine. Error wins, then an active run, then a
  // settled result; otherwise we're idle. Quick runs only count as `running`
  // once they cross the delay above, so fast ones skip the running view.
  const phase = $derived.by((): 'idle' | 'running' | 'result' | 'error' => {
    if (runError !== undefined) return 'error';
    if (aiStreaming || (pending !== undefined && quickRunningVisible)) return 'running';
    if (lastResult !== undefined) return 'result';
    return 'idle';
  });
  // Stream partials into the same area the final result uses, so the text
  // never jumps when a run completes. While a slow deterministic run shows the
  // "Working…" header, drop the previous result's body so it doesn't sit under
  // it; the fast path (no running indicator yet) keeps the prior result until
  // the new one replaces it.
  const workText = $derived.by((): string => {
    if (aiStreaming) return aiText;
    if (pending !== undefined && quickRunningVisible) return '';
    return lastResult ?? '';
  });
  const runningLabel = $derived(aiStreaming ? t.actionMenu.generating : t.actionMenu.working);

  // The localized "why is this disabled" hint for one action: its remediation
  // key, or a generic fallback. `undefined` when the action is available.
  const reasonFor = (entry: AiAvailability['actions'][number] | undefined): string | undefined => {
    if (entry?.available) return undefined;
    return entry?.remediation
      ? (t.actionMenu.aiRemediation[entry.remediation] ?? t.actionMenu.aiUnavailable)
      : t.actionMenu.aiUnavailable;
  };

  // Actions operate on an entry's text representation, so a content kind with
  // no usable text gets its button disabled (with a reason) rather than
  // silently running on an empty or meaningless string. Images carry no text;
  // file lists and bare URLs carry only incidental text (paths, the URL itself)
  // the text transforms would mangle — the lone exception is `RedactSecrets`,
  // which is exactly what you want on a URL holding a token. The daemon also
  // refuses text-less content, so this is UX, not the safety boundary.
  const actionAppliesToKind = (kind: ContentKind, id: string): boolean => {
    switch (kind) {
      case 'image':
      case 'fileList':
        return false;
      case 'url':
        return id === 'RedactSecrets';
      default:
        return true;
    }
  };

  // The localized hover hint for an action disabled because it can't run on the
  // focused entry's content kind. `undefined` for kinds we never gate.
  const inapplicableReason = (kind: ContentKind): string | undefined => {
    switch (kind) {
      case 'image':
        return t.actionMenu.notApplicable.image;
      case 'fileList':
        return t.actionMenu.notApplicable.fileList;
      case 'url':
        return t.actionMenu.notApplicable.url;
      default:
        return undefined;
    }
  };

  // One descriptor per AI text action, gated by its own availability.
  // Empty on hosts with no AI backend (today non-macOS) so the menu shows
  // only quick actions — no dead AI buttons, no "unavailable" footnote.
  const aiActionList = $derived(
    aiActionsSupported()
      ? AI_ACTION_IDS.map((action) => {
          const entry = availability?.actions.find((e) => e.action === action);
          return {
            action,
            label: t.actionMenu.aiActions[action],
            available: entry?.available ?? false,
            reason: reasonFor(entry),
          };
        })
      : [],
  );
  const anyAiAvailable = $derived(aiActionList.some((item) => item.available));
  // True when the focused entry's content kind gates every surfaced AI action
  // off (an image / file list / bare URL). The per-button "doesn't apply"
  // reason already explains that, so the AI-availability footnote below would
  // only contradict it.
  const aiGatedByKind = $derived(
    !!target && aiActionList.every((item) => !actionAppliesToKind(target.kind, item.action)),
  );
  // Shown once below the list when nothing is runnable. The text actions all
  // resolve to the same on-device backend, so the first hint represents them
  // all (e.g. "enable Apple Intelligence"). Suppressed where AI has no backend
  // (the actions are hidden there, so a footnote would be noise) and where the
  // content kind already gates every AI action off.
  const aiUnavailableReason = $derived(
    !aiActionsSupported() || anyAiAvailable || aiGatedByKind
      ? undefined
      : (aiActionList.find((item) => item.reason)?.reason ?? t.actionMenu.aiUnavailable),
  );

  const flashDone = (): void => {
    doneFlash = true;
    if (doneFlashTimer !== undefined) clearTimeout(doneFlashTimer);
    doneFlashTimer = setTimeout(() => {
      doneFlashTimer = undefined;
      doneFlash = false;
    }, DONE_FLASH_MS);
  };

  // Tear down the work area back to idle: invalidate any in-flight quick run
  // (the bumped `runToken` fences late resolutions), cancel an active AI
  // stream, and clear every transient flag and timer. Shared by the close and
  // re-target effects so both leave the same clean slate.
  const resetRun = (): void => {
    runToken += 1;
    cancelRequested = false;
    if (aiRequestId !== undefined) void cancelAiAction(aiRequestId);
    lastResult = undefined;
    runError = undefined;
    copyOk = false;
    saveOk = false;
    pending = undefined;
    aiText = '';
    aiStreaming = false;
    aiRequestId = undefined;
    aiPendingAction = undefined;
    doneFlash = false;
    quickRunningVisible = false;
    if (quickRunningTimer !== undefined) {
      clearTimeout(quickRunningTimer);
      quickRunningTimer = undefined;
    }
    if (doneFlashTimer !== undefined) {
      clearTimeout(doneFlashTimer);
      doneFlashTimer = undefined;
    }
  };

  const run = async (id: QuickActionId): Promise<void> => {
    if (!target || !isTauri() || busy) return;
    const token = ++runToken;
    pending = id;
    runError = undefined;
    copyOk = false;
    saveOk = false;
    // Arm the delayed running indicator; a fast resolve clears it first.
    if (quickRunningTimer !== undefined) clearTimeout(quickRunningTimer);
    quickRunningVisible = false;
    quickRunningTimer = setTimeout(() => {
      quickRunningTimer = undefined;
      quickRunningVisible = true;
    }, QUICK_RUNNING_DELAY_MS);
    try {
      const result = await runQuickAction(id, target.id);
      // Bail if the menu was closed (and the reset bumped `runToken`) while the
      // IPC was in flight, so a stale result can't land in a reopened menu.
      if (token !== runToken) return;
      lastResult = result.text;
    } catch (err) {
      if (token !== runToken) return;
      runError = describeError(err);
      lastResult = undefined;
    } finally {
      // Only this run owns the shared running state; if it was superseded, the
      // reset (or the newer run) already cleared it.
      if (token === runToken) {
        pending = undefined;
        if (quickRunningTimer !== undefined) {
          clearTimeout(quickRunningTimer);
          quickRunningTimer = undefined;
        }
        quickRunningVisible = false;
      }
    }
  };

  const runAiAction = async (action: AiTextActionId): Promise<void> => {
    const entry = aiActionList.find((item) => item.action === action);
    if (!target || !isTauri() || busy || !entry?.available) return;
    // Fence the startup window the same way `run` does: closing, re-targeting,
    // or starting another action bumps `runToken`, so a `startAiAction` that
    // resolves after any of those can detect it lost the race.
    const token = ++runToken;
    runError = undefined;
    lastResult = undefined;
    aiText = '';
    aiStreaming = true;
    aiPendingAction = action;
    doneFlash = false;
    cancelRequested = false;
    try {
      // Don't start the backend run until the request-scoped listeners have
      // attached; otherwise a fast `done`/`error` could fire before we are
      // listening and the UI would hang in the running state. The fast-path
      // boolean keeps the common open-then-click case synchronous; only a click
      // that lands inside the attach window pays the await. Bail if a close /
      // re-target / newer run superseded us while we waited; if the gate rejects
      // (a listener failed to attach), the `catch` below surfaces the error
      // instead of starting a run whose terminal event could never arrive.
      if (!aiListenersAttached) {
        await aiListenersReady;
        if (token !== runToken) return;
      }
      const id = await startAiAction(action, target.id);
      // Superseded while the backend was spinning up (the inspector was closed
      // or re-targeted before the id arrived): cancel the orphaned backend run
      // so it doesn't stream into a now-stale request, and don't adopt its id.
      if (token !== runToken) {
        void cancelAiAction(id);
        return;
      }
      // Cancel pressed during startup (before the id existed): honour it now
      // that the backend has a handle, and abandon this run.
      if (cancelRequested) {
        cancelRequested = false;
        void cancelAiAction(id);
        aiStreaming = false;
        aiPendingAction = undefined;
        return;
      }
      aiRequestId = id;
    } catch (err) {
      // A superseding run (or the reset) already owns the work area; don't let
      // this run's failure clobber it.
      if (token !== runToken) return;
      runError = describeError(err);
      aiStreaming = false;
      aiRequestId = undefined;
      aiPendingAction = undefined;
    }
  };

  const cancelAi = (): void => {
    if (aiRequestId !== undefined) {
      void cancelAiAction(aiRequestId);
    } else if (aiStreaming) {
      // The request id hasn't arrived yet; remember the intent so `runAiAction`
      // cancels the moment `startAiAction` resolves.
      cancelRequested = true;
    }
  };

  // One flat list of buttons: deterministic actions first, then AI actions
  // (each badged). The user scans by intent, not by section.
  const pickerItems = $derived([
    ...QUICK_ACTION_IDS.map((id) => {
      const applies = !target || actionAppliesToKind(target.kind, id);
      return {
        key: `quick-${id}`,
        label: t.actionMenu.actions[id],
        isAi: false,
        disabled: !target || busy || !applies,
        reason: target && !applies ? inapplicableReason(target.kind) : undefined,
        pending: pending === id,
        run: () => void run(id),
      };
    }),
    ...aiActionList.map((item) => {
      const applies = !target || actionAppliesToKind(target.kind, item.action);
      return {
        key: `ai-${item.action}`,
        label: item.label,
        isAi: true,
        disabled: !target || busy || !item.available || !applies,
        // A content mismatch is the more relevant explanation than the generic
        // AI-availability hint, so it wins when both apply.
        reason: target && !applies ? inapplicableReason(target.kind) : item.reason,
        pending: aiPendingAction === item.action,
        run: () => void runAiAction(item.action),
      };
    }),
  ]);

  // Reset feedback after a beat so repeated actions still flash visibly.
  // Each flag owns its own timer so a quick second click doesn't let the
  // first run's lingering timeout flip the freshly-set `true` back to `false`.
  const flashOk = async (
    setOk: (value: boolean) => void,
    timerRef: { value: FlashTimer },
    fn: () => Promise<void>,
  ): Promise<void> => {
    try {
      await fn();
      setOk(true);
      if (timerRef.value !== undefined) clearTimeout(timerRef.value);
      timerRef.value = setTimeout(() => {
        timerRef.value = undefined;
        setOk(false);
      }, FLASH_MS);
    } catch {
      setOk(false);
    }
  };

  const copyTimerRef = {
    get value() {
      return copyFlashTimer;
    },
    set value(v: FlashTimer) {
      copyFlashTimer = v;
    },
  };
  const saveTimerRef = {
    get value() {
      return saveFlashTimer;
    },
    set value(v: FlashTimer) {
      saveFlashTimer = v;
    },
  };

  const copyResult = (): Promise<void> => {
    const text = lastResult;
    if (text === undefined) return Promise.resolve();
    return flashOk(
      (v) => (copyOk = v),
      copyTimerRef,
      () => navigator.clipboard.writeText(text),
    );
  };

  const saveResult = async (): Promise<void> => {
    const text = lastResult;
    if (text === undefined || !isTauri()) return;
    saving = true;
    try {
      await flashOk(
        (v) => (saveOk = v),
        saveTimerRef,
        async () => {
          await saveAiResult(text);
        },
      );
    } finally {
      saving = false;
    }
  };

  // Escape cancels an in-flight stream first (so a stray keystroke doesn't
  // abandon the run *and* close), and only closes the inspector when idle.
  const onEscape = (): void => {
    if (aiStreaming) cancelAi();
    else onClose();
  };

  // Reset transient feedback when the user dismisses or re-opens the inspector.
  // An in-flight AI run is cancelled so the backend (and its concurrency permit)
  // is released rather than streaming on to no one.
  $effect(() => {
    if (!open) resetRun();
  });

  // Because the inspector is docked (not a modal), the user can re-target it
  // with ↑/↓ while it stays open — the palette feeds it the live selection.
  // (The list is a read-only reference surface while open: the palette freezes
  // hover selection and makes row clicks and the per-row pin button inert, so
  // none of them re-target in place or tear down the palette.) The work area
  // belongs to the *previous* target, so when the id
  // changes under an open inspector we cancel any run and clear it rather than
  // leave a stale result (or land a finishing run) against the new entry.
  // `lastSeenTargetId` tracks the id while closed too, so reopening on the same
  // entry (the common case) is not treated as a change. `untrack` keeps this
  // effect's dependencies to `open` + `target.id` only.
  let lastSeenTargetId: string | undefined = undefined;
  $effect(() => {
    const id = target?.id;
    if (!open) {
      lastSeenTargetId = id;
      return;
    }
    if (id === lastSeenTargetId) return;
    lastSeenTargetId = id;
    untrack(() => resetRun());
  });

  // Probe AI availability each time the inspector opens so the AI buttons
  // reflect the live Apple Intelligence / provider state (disabled + reason).
  // Skipped where AI has no backend: the actions are hidden there, so the
  // probe would only ever report "unavailable" to a list nobody renders.
  $effect(() => {
    if (!open || !isTauri() || !aiActionsSupported()) return;
    void (async () => {
      try {
        availability = await getAiAvailability();
      } catch {
        availability = undefined;
      }
    })();
  });

  // Subscribe to the request-scoped streaming events while the inspector is
  // open. Events whose `requestId` does not match the active run are discarded.
  $effect(() => {
    if (!open || !isTauri()) return;
    // `aiRequestId` is the id returned by `startAiAction` — authoritative and
    // scoped to *this* run, so we never adopt a stray `started` from another
    // run/window. `runAiAction` waits on `aiListenersReady` before starting, so
    // even the fastest terminal event lands after every listener has attached.
    const matches = (id: string): boolean => aiRequestId !== undefined && id === aiRequestId;
    // Arm the ready gate: resolve once every subscription's underlying
    // `listen()` has attached so a run started afterward can't miss an event,
    // or reject if any attach fails so a run fails closed instead of starting
    // with a missing listener (which would never deliver its terminal event and
    // strand the UI).
    let attached = 0;
    // Assigned synchronously by the Promise executor below before any use.
    let resolveReady!: () => void;
    let rejectReady!: (reason: unknown) => void;
    aiListenersAttached = false;
    aiListenersReady = new Promise<void>((resolve, reject) => {
      resolveReady = resolve;
      rejectReady = reject;
    });
    // Keep a rejected gate from surfacing as an unhandled rejection when no run
    // is awaiting it; a run that *does* await sees the rejection on its own.
    aiListenersReady.catch(() => {});
    const SUBSCRIPTION_COUNT = 5;
    const markAttached = (): void => {
      attached += 1;
      if (attached === SUBSCRIPTION_COUNT) {
        aiListenersAttached = true;
        resolveReady();
      }
    };
    const markFailed = (): void => {
      rejectReady(new Error('ai event listener failed to attach'));
    };
    const unsubscribers = [
      subscribe<AiDeltaEvent>(
        TAURI_EVENTS.aiDelta,
        (payload) => {
          if (matches(payload.requestId)) aiText += payload.text;
        },
        markAttached,
        markFailed,
      ),
      subscribe<AiReplaceEvent>(
        TAURI_EVENTS.aiReplace,
        (payload) => {
          if (matches(payload.requestId)) aiText = payload.text;
        },
        markAttached,
        markFailed,
      ),
      subscribe<AiDoneEvent>(
        TAURI_EVENTS.aiDone,
        (payload) => {
          if (!matches(payload.requestId)) return;
          aiText = payload.finalText;
          lastResult = payload.finalText;
          aiStreaming = false;
          aiRequestId = undefined;
          aiPendingAction = undefined;
          flashDone();
        },
        markAttached,
        markFailed,
      ),
      subscribe<AiErrorEvent>(
        TAURI_EVENTS.aiError,
        (payload) => {
          if (!matches(payload.requestId)) return;
          runError = payload.message;
          aiStreaming = false;
          aiRequestId = undefined;
          aiPendingAction = undefined;
        },
        markAttached,
        markFailed,
      ),
      subscribe<{ requestId: string }>(
        TAURI_EVENTS.aiCancelled,
        (payload) => {
          if (!matches(payload.requestId)) return;
          aiStreaming = false;
          aiRequestId = undefined;
          aiPendingAction = undefined;
        },
        markAttached,
        markFailed,
      ),
    ];
    return () => {
      // Unblock any run still awaiting this gate — it bails on its `runToken`
      // check, since closing/re-targeting bumps the token via `resetRun` — then
      // drop back to the resolved default so the next open re-arms a fresh
      // pending gate.
      resolveReady();
      aiListenersAttached = true;
      aiListenersReady = Promise.resolve();
      for (const unsub of unsubscribers) unsub();
    };
  });

  // Move focus into the panel on open so screen readers announce the region and
  // so the Escape keydown handler below has somewhere reachable to fire from.
  $effect(() => {
    if (open && panelEl) {
      panelEl.focus();
    }
  });

  // Starting an AI stream disables the action button that launched it, so focus
  // would otherwise land on a disabled control (or <body>) and route Escape
  // past the panel (to the palette's window handler) instead of into our
  // cancel logic. Pull focus back to the panel when a stream begins so it
  // keeps owning the keyboard. Gated on `aiStreaming` (not `busy`) so quick
  // sub-150ms runs don't yank focus on every click.
  $effect(() => {
    if (aiStreaming && panelEl) {
      panelEl.focus();
    }
  });

  // Cancel any in-flight timers on destroy so they don't fire a state write
  // into a component that no longer has a consumer.
  $effect(() => {
    return () => {
      if (copyFlashTimer !== undefined) clearTimeout(copyFlashTimer);
      if (saveFlashTimer !== undefined) clearTimeout(saveFlashTimer);
      if (quickRunningTimer !== undefined) clearTimeout(quickRunningTimer);
      if (doneFlashTimer !== undefined) clearTimeout(doneFlashTimer);
    };
  });
</script>

{#if open}
  <!-- A docked panel, not a modal: it shares the palette body with the result
       list rather than floating over it, so the target the actions run against
       stays in view beside its source row. `role="dialog"` without `aria-modal`
       marks it as a non-modal task surface — focusable, labelled, and
       Escape-dismissible, but it neither traps focus nor blocks the list the way
       the old modal did. While focused the panel owns the keyboard (keydowns are
       stopped before the palette's window handler) so action shortcuts don't
       leak and Escape routes into `onEscape`. A plain <div> carries the role
       cleanly (a landmark <aside> cannot host it). -->
  <div
    class="action-inspector"
    data-testid="action-inspector"
    role="dialog"
    aria-labelledby="action-inspector-title"
    tabindex="-1"
    bind:this={panelEl}
    onkeydown={(e) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onEscape();
        return;
      }
      // The open-actions chord pressed while the inspector holds focus toggles
      // it closed. Swallow it so the window-level handler can't re-open it on
      // the same keystroke, and dismiss outright: closing cancels any in-flight
      // run (via the `open` effect), unlike Escape which only cancels the
      // stream and keeps the panel up.
      if (matchesToggle?.(e)) {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      e.stopPropagation();
    }}
  >
    <header class="head">
      <span id="action-inspector-title">{t.actionMenu.title}</span>
      <button type="button" class="close" aria-label={t.actionMenu.close} onclick={onClose}
        >×</button
      >
    </header>

    <!-- Everything above the work area is one scroll region. When a run/result
         claims the panel below, this block yields its space (it shrinks first)
         and scrolls its own overflow rather than letting the result collapse or
         the panel clip — the picker stays fully reachable while the result
         keeps a readable floor. -->
    <div class="controls">
      <CompactPreview item={target} compact={phase !== 'idle'} />

      <div class="divider"></div>

      <ActionPicker items={pickerItems} aiBadge={t.actionMenu.aiBadge} compact={phase !== 'idle'} />

      {#if aiUnavailableReason}
        <p class="ai-reason">{aiUnavailableReason}</p>
      {/if}
    </div>

    <ActionRunPanel
      {phase}
      text={workText}
      streaming={aiStreaming}
      {runningLabel}
      errorMessage={runError}
      {doneFlash}
      labels={{
        result: t.actionMenu.resultTitle,
        copy: t.actionMenu.copyResult,
        copied: t.actionMenu.copied,
        save: t.actionMenu.saveResult,
        saved: t.actionMenu.saved,
        cancel: t.actionMenu.aiCancel,
        done: t.actionMenu.done,
      }}
      {copyOk}
      {saveOk}
      {saving}
      canSave={isTauri()}
      onCopy={() => void copyResult()}
      onSave={() => void saveResult()}
      onCancel={cancelAi}
    />

    {#if !isTauri()}
      <p class="hint">{t.actionMenu.tauriRequired}</p>
    {/if}
  </div>
{/if}

<style>
  .action-inspector {
    display: flex;
    flex-direction: column;
    gap: 0.625rem;
    /* Share the palette body with the result list. `flex: 1` splits the width
       with the list; full panel height (vs. the old 80vh modal) is what gives
       long results room to breathe. */
    flex: 1;
    min-width: 0;
    min-height: 0;
    padding: 1rem;
    border-left: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    background: var(--bg-elevated, rgba(255, 255, 255, 0.02));
    color: var(--fg, #f5f5f5);
    /* The two inner regions own their scroll (`.controls` and the result
       `<pre>`), so the panel frame itself stays put. */
    overflow: hidden;
  }
  .controls {
    display: flex;
    flex-direction: column;
    gap: 0.625rem;
    /* Shrinks before the work area does (grow 0, shrink 1) and scrolls its
       own overflow, so the picker is always reachable without ever stealing
       the result's room or clipping against the panel edge. */
    flex: 0 1 auto;
    min-height: 0;
    overflow-y: auto;
  }
  .action-inspector:focus {
    outline: none;
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 0.9375rem;
    font-weight: 600;
    color: var(--fg, #f5f5f5);
  }
  .close {
    width: 1.75rem;
    height: 1.75rem;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 1.1rem;
    cursor: pointer;
  }
  .close:hover {
    background: color-mix(in srgb, var(--fg, #f5f5f5) 8%, transparent);
  }
  .close:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  .divider {
    height: 1px;
    background: var(--border, rgba(255, 255, 255, 0.08));
  }
  .ai-reason {
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .hint {
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
</style>

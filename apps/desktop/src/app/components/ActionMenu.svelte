<script lang="ts">
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
    QuickActionId,
    SearchResultDto,
  } from '../lib/types';
  import ActionPicker from './ActionPicker.svelte';
  import ActionRunPanel from './ActionRunPanel.svelte';
  import CompactPreview from './CompactPreview.svelte';

  type Props = {
    target: SearchResultDto | undefined;
    open: boolean;
    onClose: () => void;
  };

  const { target, open, onClose }: Props = $props();

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
  let menuEl: HTMLDivElement | undefined = $state();

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
  // run that resolves after the menu was closed (and possibly reopened on a
  // different target) can't commit stale output. `cancelRequested` remembers a
  // cancel pressed during the AI startup window — before `startAiAction` has
  // returned a request id — so we can cancel the moment that id arrives.
  let runToken = 0;
  let cancelRequested = false;

  type FlashTimer = ReturnType<typeof setTimeout> | undefined;
  let copyFlashTimer: FlashTimer = undefined;
  let saveFlashTimer: FlashTimer = undefined;

  // A run is in flight whenever a quick action or an AI stream is active; the
  // whole picker disables so a second action can't race the first.
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

  // One descriptor per AI text action, gated by its own availability.
  const aiActionList = $derived(
    AI_ACTION_IDS.map((action) => {
      const entry = availability?.actions.find((e) => e.action === action);
      return {
        action,
        label: t.actionMenu.aiActions[action],
        available: entry?.available ?? false,
        reason: reasonFor(entry),
      };
    }),
  );
  const anyAiAvailable = $derived(aiActionList.some((item) => item.available));
  // Shown once below the list when nothing is runnable. The text actions all
  // resolve to the same on-device backend, so the first hint represents them
  // all (e.g. "enable Apple Intelligence").
  const aiUnavailableReason = $derived(
    anyAiAvailable
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
    runError = undefined;
    lastResult = undefined;
    aiText = '';
    aiStreaming = true;
    aiPendingAction = action;
    doneFlash = false;
    cancelRequested = false;
    try {
      const id = await startAiAction(action, target.id);
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
    ...QUICK_ACTION_IDS.map((id) => ({
      key: `quick-${id}`,
      label: t.actionMenu.actions[id],
      isAi: false,
      disabled: !target || busy,
      pending: pending === id,
      run: () => void run(id),
    })),
    ...aiActionList.map((item) => ({
      key: `ai-${item.action}`,
      label: item.label,
      isAi: true,
      disabled: !target || busy || !item.available,
      reason: item.reason,
      pending: aiPendingAction === item.action,
      run: () => void runAiAction(item.action),
    })),
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
  // abandon the run *and* close), and only closes the menu when idle.
  const onEscape = (): void => {
    if (aiStreaming) cancelAi();
    else onClose();
  };

  // Reset transient feedback when the user dismisses or re-opens the menu. An
  // in-flight AI run is cancelled so the backend (and its concurrency permit)
  // is released rather than streaming on to no one.
  $effect(() => {
    if (!open) {
      // Invalidate any in-flight quick run and pending startup-cancel so their
      // late resolutions don't write into a closed (or reopened) menu.
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
    }
  });

  // Probe AI availability each time the menu opens so the AI buttons reflect
  // the live Apple Intelligence / provider state (disabled + reason when off).
  $effect(() => {
    if (!open || !isTauri()) return;
    void (async () => {
      try {
        availability = await getAiAvailability();
      } catch {
        availability = undefined;
      }
    })();
  });

  // Subscribe to the request-scoped streaming events while the menu is open.
  // Events whose `requestId` does not match the active run are discarded.
  $effect(() => {
    if (!open || !isTauri()) return;
    // `aiRequestId` is the id returned by `startAiAction` — authoritative and
    // scoped to *this* run, so we never adopt a stray `started` from another
    // run/window. The command returns the id before the backend produces its
    // first real snapshot, so deltas are not dropped in practice.
    const matches = (id: string): boolean => aiRequestId !== undefined && id === aiRequestId;
    const unsubscribers = [
      subscribe<AiDeltaEvent>(TAURI_EVENTS.aiDelta, (payload) => {
        if (matches(payload.requestId)) aiText += payload.text;
      }),
      subscribe<AiReplaceEvent>(TAURI_EVENTS.aiReplace, (payload) => {
        if (matches(payload.requestId)) aiText = payload.text;
      }),
      subscribe<AiDoneEvent>(TAURI_EVENTS.aiDone, (payload) => {
        if (!matches(payload.requestId)) return;
        aiText = payload.finalText;
        lastResult = payload.finalText;
        aiStreaming = false;
        aiRequestId = undefined;
        aiPendingAction = undefined;
        flashDone();
      }),
      subscribe<AiErrorEvent>(TAURI_EVENTS.aiError, (payload) => {
        if (!matches(payload.requestId)) return;
        runError = payload.message;
        aiStreaming = false;
        aiRequestId = undefined;
        aiPendingAction = undefined;
      }),
      subscribe<{ requestId: string }>(TAURI_EVENTS.aiCancelled, (payload) => {
        if (!matches(payload.requestId)) return;
        aiStreaming = false;
        aiRequestId = undefined;
        aiPendingAction = undefined;
      }),
    ];
    return () => {
      for (const unsub of unsubscribers) unsub();
    };
  });

  // Move focus into the dialog on open so screen readers announce the role and
  // so the Escape keydown handler below has somewhere reachable to fire from.
  $effect(() => {
    if (open && menuEl) {
      menuEl.focus();
    }
  });

  // Starting an AI stream disables the action button that launched it, so focus
  // would otherwise land on a disabled control (or <body>) and route Escape
  // past the dialog (to the palette's window handler) instead of into our
  // cancel logic. Pull focus back to the dialog when a stream begins so it
  // keeps owning the keyboard. Gated on `aiStreaming` (not `busy`) so quick
  // sub-150ms runs don't yank focus on every click.
  $effect(() => {
    if (aiStreaming && menuEl) {
      menuEl.focus();
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
  <div
    class="scrim"
    role="presentation"
    onclick={onClose}
    onkeydown={(e) => {
      if (e.key === 'Escape') onClose();
    }}
  >
    <div
      class="menu"
      role="dialog"
      aria-modal="true"
      tabindex="-1"
      aria-labelledby="action-menu-title"
      bind:this={menuEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => {
        // The dialog stops keydown from leaking out so action button
        // shortcuts don't bubble into the palette behind. Escape still has to
        // be handled here directly.
        if (e.key === 'Escape') {
          e.stopPropagation();
          onEscape();
          return;
        }
        e.stopPropagation();
      }}
    >
      <header class="head">
        <span id="action-menu-title">{t.actionMenu.title}</span>
        <button type="button" class="close" onclick={onClose}>×</button>
      </header>

      <CompactPreview item={target} />

      <div class="divider"></div>

      <ActionPicker items={pickerItems} aiBadge={t.actionMenu.aiBadge} compact={phase !== 'idle'} />

      {#if aiUnavailableReason}
        <p class="ai-reason">{aiUnavailableReason}</p>
      {/if}

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
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.45);
  }
  .menu {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    width: min(480px, 92vw);
    max-height: 80vh;
    padding: 1rem;
    border-radius: 8px;
    background: var(--bg-overlay, #1d1f23);
    color: var(--fg, #f5f5f5);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5);
    overflow: auto;
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

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

  type Props = {
    target: SearchResultDto | undefined;
    open: boolean;
    onClose: () => void;
    // Soft-delete every non-pinned entry. Wired by the palette to the
    // `clearAllHistory` store action; absent in standalone/test mounts.
    onClearAll?: () => void;
  };

  const { target, open, onClose, onClearAll }: Props = $props();

  // The deterministic on-device quick actions. The model-backed "AI:
  // Summarize" entry below is a separate path that streams.
  const QUICK_ACTION_IDS: readonly QuickActionId[] = [
    'SummarizeFirstSentence',
    'FormatJson',
    'ExtractTasks',
    'RedactSecrets',
  ];

  const t = $derived(messages());

  let pending: QuickActionId | undefined = $state(undefined);
  let lastResult: string | undefined = $state(undefined);
  let runError: string | undefined = $state(undefined);
  let copyOk = $state(false);
  let saveOk = $state(false);
  let saving = $state(false);
  let menuEl: HTMLDivElement | undefined = $state();

  // Streaming AI state. `aiRequestId` scopes the `nagori://ai/*` events we
  // accept; `aiText` is the request-local display buffer.
  let availability = $state<AiAvailability | undefined>(undefined);
  let aiRequestId = $state<string | undefined>(undefined);
  let aiText = $state('');
  let aiStreaming = $state(false);

  const summarizeAvailability = $derived(
    availability?.actions.find((entry) => entry.action === 'Summarize'),
  );
  const summarizeAvailable = $derived(summarizeAvailability?.available ?? false);
  // Tooltip text for a disabled AI button: the localized remediation hint, or
  // a generic "unavailable" fallback keyed off the per-action status.
  const summarizeReason = $derived(
    summarizeAvailable
      ? undefined
      : summarizeAvailability?.remediation
        ? (t.actionMenu.aiRemediation[summarizeAvailability.remediation] ??
          t.actionMenu.aiUnavailable)
        : t.actionMenu.aiUnavailable,
  );

  // Reset transient feedback when the user dismisses or re-opens the menu. An
  // in-flight AI run is cancelled so the backend (and its concurrency permit)
  // is released rather than streaming on to no one.
  $effect(() => {
    if (!open) {
      if (aiRequestId !== undefined) void cancelAiAction(aiRequestId);
      lastResult = undefined;
      runError = undefined;
      copyOk = false;
      saveOk = false;
      pending = undefined;
      aiText = '';
      aiStreaming = false;
      aiRequestId = undefined;
    }
  });

  // Probe AI availability each time the menu opens so the AI button reflects
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
      }),
      subscribe<AiErrorEvent>(TAURI_EVENTS.aiError, (payload) => {
        if (!matches(payload.requestId)) return;
        runError = payload.message;
        aiStreaming = false;
        aiRequestId = undefined;
      }),
      subscribe<{ requestId: string }>(TAURI_EVENTS.aiCancelled, (payload) => {
        if (!matches(payload.requestId)) return;
        aiStreaming = false;
        aiRequestId = undefined;
      }),
    ];
    return () => {
      for (const unsub of unsubscribers) unsub();
    };
  });

  const runAiSummarize = async (): Promise<void> => {
    if (!target || !isTauri() || aiStreaming || !summarizeAvailable) return;
    runError = undefined;
    lastResult = undefined;
    aiText = '';
    aiStreaming = true;
    try {
      aiRequestId = await startAiAction('Summarize', target.id);
    } catch (err) {
      runError = describeError(err);
      aiStreaming = false;
      aiRequestId = undefined;
    }
  };

  const cancelAi = (): void => {
    if (aiRequestId !== undefined) void cancelAiAction(aiRequestId);
  };

  // Move focus into the dialog on open so screen readers announce the
  // role and so the Escape keydown handler below has somewhere reachable
  // to fire from. Without this the keyboard focus stays on whatever
  // triggered the menu and the dialog feels untethered.
  $effect(() => {
    if (open && menuEl) {
      menuEl.focus();
    }
  });

  const run = async (id: QuickActionId): Promise<void> => {
    if (!target || !isTauri()) return;
    pending = id;
    runError = undefined;
    copyOk = false;
    saveOk = false;
    try {
      const result = await runQuickAction(id, target.id);
      lastResult = result.text;
    } catch (err) {
      runError = describeError(err);
      lastResult = undefined;
    } finally {
      pending = undefined;
    }
  };

  // Clear the whole (non-pinned) history. Closing the menu immediately is
  // the feedback here — the palette list re-runs its query underneath and
  // the cleared rows vanish; there is no confirmation dialog, matching the
  // tray "Clear History" item. We intentionally do NOT gate on the visible
  // result count: that reflects the active query/filter, not the global
  // non-pinned history this clears, so an empty filtered view must not
  // disable a global action. An already-empty history is a harmless no-op
  // (the daemon returns 0).
  const clearAll = (): void => {
    if (!isTauri() || !onClearAll) return;
    onClearAll();
    onClose();
  };

  // Reset feedback after a beat so repeated actions still flash visibly.
  // Each flag owns its own timer so a quick second click doesn't let the
  // first run's lingering timeout flip the freshly-set `true` back to
  // `false`. Cleared on unmount to avoid post-destroy state writes.
  const FLASH_MS = 1500;
  type FlashTimer = ReturnType<typeof setTimeout> | undefined;
  let copyFlashTimer: FlashTimer = undefined;
  let saveFlashTimer: FlashTimer = undefined;

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

  // Cancel any in-flight flash timers on destroy so they don't fire
  // setOk(false) into a state that no longer has a consumer.
  $effect(() => {
    return () => {
      if (copyFlashTimer !== undefined) clearTimeout(copyFlashTimer);
      if (saveFlashTimer !== undefined) clearTimeout(saveFlashTimer);
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
      tabindex="-1"
      aria-label={t.actionMenu.title}
      bind:this={menuEl}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => {
        // The dialog stops keydown from leaking out so action button
        // shortcuts don't bubble into the palette behind. Escape still
        // has to close the menu, so handle it here directly.
        if (e.key === 'Escape') {
          e.stopPropagation();
          onClose();
          return;
        }
        e.stopPropagation();
      }}
    >
      <header class="head">
        <span>{t.actionMenu.title}</span>
        <button type="button" class="close" onclick={onClose}>×</button>
      </header>
      <ul class="list">
        {#each QUICK_ACTION_IDS as id (id)}
          <li>
            <button
              type="button"
              disabled={!target || pending !== undefined}
              onclick={() => run(id)}
            >
              {t.actionMenu.actions[id]}
              {#if pending === id}<span class="pending">…</span>{/if}
            </button>
          </li>
        {/each}
      </ul>

      <section class="ai" aria-label={t.actionMenu.aiTitle}>
        <header class="ai-head">{t.actionMenu.aiTitle}</header>
        <button
          type="button"
          class="ai-button"
          disabled={!target || aiStreaming || pending !== undefined || !summarizeAvailable}
          title={summarizeReason}
          onclick={() => void runAiSummarize()}
        >
          {t.actionMenu.aiSummarize}
          {#if aiStreaming}<span class="pending">…</span>{/if}
        </button>
        {#if aiStreaming}
          <button type="button" class="ghost ai-cancel" onclick={cancelAi}>
            {t.actionMenu.aiCancel}
          </button>
          <pre class="result ai-stream">{aiText}</pre>
        {/if}
        {#if summarizeReason}
          <p class="ai-reason">{summarizeReason}</p>
        {/if}
      </section>

      {#if onClearAll}
        <section class="danger" aria-label={t.actionMenu.clearAllHistory}>
          <button type="button" class="danger-button" disabled={!isTauri()} onclick={clearAll}>
            {t.actionMenu.clearAllHistory}
          </button>
          <p class="danger-hint">{t.actionMenu.clearAllHistoryHint}</p>
        </section>
      {/if}

      {#if runError}
        <p class="error">{runError}</p>
      {/if}

      {#if lastResult !== undefined}
        <section class="result-block" aria-label={t.actionMenu.resultTitle}>
          <header class="result-head">
            <span>{t.actionMenu.resultTitle}</span>
            <div class="result-actions">
              <button type="button" class="ghost" onclick={() => void copyResult()}>
                {copyOk ? t.actionMenu.copied : t.actionMenu.copyResult}
              </button>
              <button
                type="button"
                class="ghost"
                disabled={saving || !isTauri()}
                onclick={() => void saveResult()}
              >
                {saveOk ? t.actionMenu.saved : t.actionMenu.saveResult}
              </button>
            </div>
          </header>
          <pre class="result">{lastResult}</pre>
          <footer class="result-foot">
            <button type="button" class="link" onclick={onClose}>
              {t.actionMenu.closeResult}
            </button>
          </footer>
        </section>
      {/if}

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
    width: min(480px, 92vw);
    max-height: 80vh;
    padding: 1rem;
    border-radius: 12px;
    background: var(--bg-overlay, #1d1f23);
    color: var(--fg, #f5f5f5);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5);
    overflow: auto;
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.75rem;
    font-size: 0.875rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted, rgba(255, 255, 255, 0.5));
  }
  .close {
    width: 1.75rem;
    height: 1.75rem;
    border: none;
    background: transparent;
    color: inherit;
    font-size: 1.1rem;
    cursor: pointer;
  }
  .list {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 0.5rem;
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .list button {
    width: 100%;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 8px;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .list button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .pending {
    margin-left: 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
  }
  .ai {
    margin-top: 0.75rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  }
  .ai-head {
    margin-bottom: 0.4rem;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted, rgba(255, 255, 255, 0.5));
  }
  .ai-button {
    width: 100%;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--accent, #6ea8fe);
    border-radius: 8px;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .ai-button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .ai-cancel {
    margin-top: 0.4rem;
  }
  .ai-stream {
    margin-top: 0.4rem;
  }
  .ai-reason {
    margin: 0.4rem 0 0;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .danger {
    margin-top: 0.75rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  }
  .danger-button {
    width: 100%;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--danger, #f87171);
    border-radius: 8px;
    background: transparent;
    color: var(--danger, #f87171);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .danger-button:not(:disabled):hover {
    background: rgba(248, 113, 113, 0.12);
  }
  .danger-button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .danger-hint {
    margin: 0.4rem 0 0;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .error {
    margin: 0.75rem 0 0;
    padding: 0.4rem 0.6rem;
    border-radius: 6px;
    background: rgba(248, 113, 113, 0.12);
    color: var(--danger, #f87171);
    font-size: 0.8125rem;
  }
  .result-block {
    margin-top: 0.75rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 8px;
    overflow: hidden;
  }
  .result-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    background: rgba(255, 255, 255, 0.04);
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .result-actions {
    display: flex;
    gap: 0.4rem;
  }
  .ghost {
    padding: 0.25rem 0.6rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 6px;
    background: transparent;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .ghost:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .result {
    margin: 0;
    padding: 0.75rem;
    background: var(--bg-code, rgba(0, 0, 0, 0.25));
    color: var(--fg, #f5f5f5);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8125rem;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 280px;
    overflow: auto;
  }
  .result-foot {
    display: flex;
    justify-content: flex-end;
    padding: 0.4rem 0.75rem;
    background: rgba(255, 255, 255, 0.02);
  }
  .link {
    border: none;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font: inherit;
    cursor: pointer;
    text-decoration: underline;
  }
  .hint {
    margin-top: 0.5rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
</style>

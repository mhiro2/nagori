<script lang="ts">
  import { clearHistory, setConfirmClearHistory } from '../lib/commands';
  import { describeError } from '../lib/errors';

  type Labels = {
    title: string;
    description: string;
    undoWarning: string;
    dontAskAgain: string;
    cancel: string;
    confirm: string;
    failed: string;
  };

  type Props = {
    labels: Labels;
    // Runs after the backend reports the history cleared, so the caller can
    // re-run its query. Not called when the clear failed.
    onCleared: () => void;
    onClose: () => void;
  };

  let { labels, onCleared, onClose }: Props = $props();

  let clearing = $state(false);
  let clearError = $state<string | undefined>(undefined);
  let dontAskAgain = $state(false);
  let dialogEl = $state<HTMLDivElement | undefined>(undefined);

  // Focus the dialog on mount so screen readers announce the role, Escape
  // fires from a reachable element, and Tab stays inside the dialog rather
  // than landing on the result list behind it. Mirrors
  // PreviewUrlConfirmDialog.svelte.
  $effect(() => {
    if (dialogEl) {
      dialogEl.focus();
    }
  });

  async function performClear(): Promise<void> {
    clearing = true;
    clearError = undefined;
    try {
      // Persist the suppression *before* clearing so the choice survives even
      // if the clear itself fails — the user's answer to "stop asking me" is
      // independent of whether this particular clear worked. A failure to
      // record it must not block the clear, so it is swallowed: the worst case
      // is being asked again next time.
      if (dontAskAgain) {
        try {
          await setConfirmClearHistory(false);
        } catch {
          // Intentionally ignored — see above.
        }
      }
      await clearHistory();
      onCleared();
      onClose();
    } catch (err) {
      clearError = describeError(err) || labels.failed;
    } finally {
      clearing = false;
    }
  }
</script>

<!-- Confirm modal for the destructive clear. Clearing is irreversible (the
     rows are hidden immediately and physically reclaimed moments later), so
     the dialog states that plainly and defaults the suppression box to off. -->
<div
  class="confirm-overlay"
  role="dialog"
  tabindex="-1"
  aria-modal="true"
  aria-labelledby="clear-history-confirm-title"
  aria-describedby="clear-history-confirm-desc"
  data-testid="clear-history-confirm"
  bind:this={dialogEl}
  onkeydown={(e) => {
    // Trap keys inside the dialog: the palette behind would otherwise act on
    // Escape (close the window) and Enter (paste the selected entry).
    if (e.key === 'Escape') {
      e.stopPropagation();
      if (!clearing) {
        onClose();
      }
      return;
    }
    // Enter confirms only while focus is on the dialog scaffold itself. Once
    // Tab has moved focus onto a button, fall through so the browser activates
    // that button and the Cancel path is honoured.
    if (e.key === 'Enter' && !clearing && e.target === dialogEl) {
      e.stopPropagation();
      e.preventDefault();
      void performClear();
    }
  }}
>
  <div class="confirm-card">
    <h3 id="clear-history-confirm-title">{labels.title}</h3>
    <p id="clear-history-confirm-desc">{labels.description}</p>
    <p class="warning">{labels.undoWarning}</p>
    {#if clearError}
      <p class="error" role="alert">{clearError}</p>
    {/if}
    <label class="suppress">
      <input
        type="checkbox"
        data-testid="clear-history-confirm-suppress"
        bind:checked={dontAskAgain}
        disabled={clearing}
      />
      {labels.dontAskAgain}
    </label>
    <div class="confirm-actions">
      <button
        type="button"
        class="secondary"
        data-testid="clear-history-confirm-cancel"
        disabled={clearing}
        onclick={onClose}
      >
        {labels.cancel}
      </button>
      <button
        type="button"
        class="danger"
        data-testid="clear-history-confirm-clear"
        disabled={clearing}
        onclick={performClear}
      >
        {labels.confirm}
      </button>
    </div>
  </div>
</div>

<style>
  .confirm-overlay {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.55);
    z-index: 50;
  }
  .confirm-card {
    width: min(420px, 90vw);
    padding: 1.25rem;
    border-radius: 8px;
    background: var(--bg, #1a1a1a);
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    color: var(--fg, #f5f5f5);
    box-shadow: 0 18px 48px rgba(0, 0, 0, 0.45);
  }
  .confirm-card h3 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }
  .confirm-card p {
    margin: 0 0 0.5rem;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
    font-size: 0.875rem;
    overflow-wrap: anywhere;
  }
  .confirm-card p.warning {
    margin-bottom: 0.75rem;
  }
  .confirm-card p.error {
    color: var(--danger, #f87171);
  }
  .suppress {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-bottom: 0.75rem;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
    font-size: 0.8125rem;
    cursor: pointer;
  }
  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }
  .confirm-actions button {
    padding: 0.35rem 0.85rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.16));
    border-radius: 4px;
    background: transparent;
    color: var(--fg, #f5f5f5);
    font: inherit;
    font-size: 0.8125rem;
    cursor: pointer;
  }
  .confirm-actions button.danger {
    background: var(--danger, #f87171);
    border-color: transparent;
    color: var(--bg, #1a1a1a);
    font-weight: 600;
  }
  .confirm-actions button[disabled] {
    opacity: 0.5;
    cursor: progress;
  }
</style>

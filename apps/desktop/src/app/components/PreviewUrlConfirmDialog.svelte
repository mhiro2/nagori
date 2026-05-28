<script lang="ts">
  import { openUrlExternal } from '../lib/commands';

  type UrlBody = {
    type: 'url';
    url: string;
    domain?: string | null;
    hostDisplay?: string | null;
  };

  type Labels = {
    title: string;
    description: (args: { host: string }) => string;
    cancel: string;
    confirm: string;
    openFailed: string;
  };

  type Props = {
    entryId: string;
    body: UrlBody;
    labels: Labels;
    onClose: () => void;
  };

  let { entryId, body, labels, onClose }: Props = $props();

  let openingUrl = $state(false);
  let openUrlError = $state<string | undefined>(undefined);
  let dialogEl = $state<HTMLDivElement | undefined>(undefined);

  // Move keyboard focus into the dialog on mount so screen readers
  // announce the role, Escape can fire from a reachable element, and
  // tab navigation lands inside the dialog rather than on a button
  // behind it. Mirrors the pattern in ActionMenu.svelte.
  $effect(() => {
    if (dialogEl) {
      dialogEl.focus();
    }
  });

  async function performOpenUrl(): Promise<void> {
    openingUrl = true;
    openUrlError = undefined;
    try {
      await openUrlExternal(entryId, body.url);
      onClose();
    } catch (err) {
      openUrlError = (err as { message?: string } | null)?.message ?? labels.openFailed;
    } finally {
      openingUrl = false;
    }
  }
</script>

<!-- Confirm modal: host display lives in the description so the user can
     compare it against the row above before the OS handler takes over.
     The renderer-side scheme gate already hides the trigger for
     non-allowlisted schemes, but the backend re-validates so this
     dialog can never produce a forged invoke that bypasses the gate. -->
<div
  class="confirm-overlay"
  role="dialog"
  tabindex="-1"
  aria-modal="true"
  aria-labelledby="preview-url-confirm-title"
  aria-describedby="preview-url-confirm-desc"
  data-testid="preview-url-confirm"
  bind:this={dialogEl}
  onkeydown={(e) => {
    // Trap keyboard events inside the dialog so they cannot bubble
    // into the palette behind. Escape closes the dialog first; the
    // global Escape handler in App.svelte would otherwise close
    // the whole preview window.
    if (e.key === 'Escape') {
      e.stopPropagation();
      if (!openingUrl) {
        onClose();
      }
      return;
    }
    // Enter on the dialog scaffold itself (initial focus target)
    // confirms — matches the "Enter to open" hint that triggered
    // this dialog. When focus has Tab-moved to a button, fall
    // through so the browser's native button activation runs and
    // the Cancel path is honoured. The Enter window-listener
    // short-circuits when the dialog is already open.
    if (e.key === 'Enter' && !openingUrl && e.target === dialogEl) {
      e.stopPropagation();
      e.preventDefault();
      void performOpenUrl();
    }
  }}
>
  <div class="confirm-card">
    <h3 id="preview-url-confirm-title">{labels.title}</h3>
    <p id="preview-url-confirm-desc">
      {labels.description({
        host: body.hostDisplay ?? body.domain ?? body.url,
      })}
    </p>
    {#if openUrlError}
      <p class="error" role="alert">{openUrlError}</p>
    {/if}
    <div class="confirm-actions">
      <button
        type="button"
        class="secondary"
        data-testid="preview-url-confirm-cancel"
        disabled={openingUrl}
        onclick={onClose}
      >
        {labels.cancel}
      </button>
      <button
        type="button"
        class="primary"
        data-testid="preview-url-confirm-open"
        disabled={openingUrl}
        onclick={performOpenUrl}
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
    margin: 0 0 0.75rem;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
    font-size: 0.875rem;
    overflow-wrap: anywhere;
  }
  .confirm-card p.error {
    color: var(--danger, #f87171);
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
  .confirm-actions button.primary {
    background: var(--syntax-link, #7ec8ff);
    border-color: transparent;
    color: var(--bg, #1a1a1a);
    font-weight: 600;
  }
  .confirm-actions button[disabled] {
    opacity: 0.5;
    cursor: progress;
  }
</style>

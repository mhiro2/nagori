<script lang="ts">
  // The shared work area for every action outcome. Deterministic results,
  // streaming AI partials, and the final AI text all land in the same `<pre>`
  // so the result never jumps position between "running" and "done". The
  // parent drives the phase; this component only renders it.
  type RunPhase = 'idle' | 'running' | 'result' | 'error';

  type Labels = {
    result: string;
    copy: string;
    copied: string;
    save: string;
    saved: string;
    cancel: string;
    done: string;
  };

  type Props = {
    phase: RunPhase;
    // Partial text while streaming, final text once done.
    text: string;
    // True only while an AI run is actively streaming (drives the pulsing dot
    // and the Cancel affordance — deterministic runs are not cancellable).
    streaming: boolean;
    runningLabel: string;
    errorMessage?: string | undefined;
    // Briefly swaps the "Result" heading for "Done" right after completion.
    doneFlash: boolean;
    labels: Labels;
    copyOk: boolean;
    saveOk: boolean;
    saving: boolean;
    canSave: boolean;
    onCopy: () => void;
    onSave: () => void;
    onCancel: () => void;
  };

  const {
    phase,
    text,
    streaming,
    runningLabel,
    errorMessage,
    doneFlash,
    labels,
    copyOk,
    saveOk,
    saving,
    canSave,
    onCopy,
    onSave,
    onCancel,
  }: Props = $props();
</script>

{#if phase !== 'idle'}
  <section class="run" data-testid="action-run">
    {#if phase === 'error'}
      <p class="error" role="alert">{errorMessage}</p>
    {:else}
      <header class="run-head">
        {#if phase === 'running'}
          <span class="status">
            <span class="dot" aria-hidden="true"></span>
            {runningLabel}
          </span>
          {#if streaming}
            <button type="button" class="ghost" onclick={onCancel}>{labels.cancel}</button>
          {/if}
        {:else}
          <span class="status">{doneFlash ? labels.done : labels.result}</span>
          <div class="run-actions">
            <button type="button" class="ghost" onclick={onCopy}>
              {copyOk ? labels.copied : labels.copy}
            </button>
            <button type="button" class="ghost" disabled={saving || !canSave} onclick={onSave}>
              {saveOk ? labels.saved : labels.save}
            </button>
          </div>
        {/if}
      </header>
      {#if text}
        <pre class="result" data-testid="action-result">{text}</pre>
      {/if}
    {/if}
  </section>
{/if}

<style>
  .run {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    /* Grow into the remaining inspector height so a long result fills the
       panel and scrolls inside the `<pre>` rather than capping early. */
    flex: 1;
    min-height: 0;
  }
  .run-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
  }
  .status {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.8125rem;
    font-weight: 600;
  }
  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--accent, #6c8dff);
    animation: pulse 1.1s ease-in-out infinite;
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.3;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
  .run-actions {
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
    font-size: 0.8125rem;
    cursor: pointer;
  }
  .ghost:hover:not(:disabled) {
    background: color-mix(in srgb, var(--fg, #f5f5f5) 8%, transparent);
  }
  .ghost:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  .ghost:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .result {
    margin: 0;
    padding: 0.875rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 6px;
    background: var(--bg-code, rgba(0, 0, 0, 0.25));
    color: var(--fg, #f5f5f5);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8125rem;
    line-height: 1.55;
    white-space: pre-wrap;
    word-break: break-word;
    /* Fill the grown `.run` area; scroll internally once the text overflows.
       `min-height` keeps a useful floor when the panel is short. */
    flex: 1;
    min-height: 6rem;
    overflow: auto;
  }
  .error {
    margin: 0;
    padding: 0.5rem 0.7rem;
    border-radius: 6px;
    background: color-mix(in srgb, var(--danger, #f87171) 14%, transparent);
    color: var(--danger, #f87171);
    font-size: 0.8125rem;
  }
</style>

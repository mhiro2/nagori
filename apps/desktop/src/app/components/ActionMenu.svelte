<script lang="ts">
  import { runAiAction, saveAiResult } from "../lib/commands";
  import { messages } from "../lib/i18n/index.svelte";
  import { isTauri } from "../lib/tauri";
  import type { AiActionId, SearchResultDto } from "../lib/types";

  type Props = {
    target: SearchResultDto | undefined;
    open: boolean;
    onClose: () => void;
  };

  const { target, open, onClose }: Props = $props();

  const ACTION_IDS: readonly AiActionId[] = [
    "Summarize",
    "Translate",
    "FormatJson",
    "FormatMarkdown",
    "ExplainCode",
    "Rewrite",
    "ExtractTasks",
    "RedactSecrets",
  ];

  const t = $derived(messages());

  let pending: AiActionId | undefined = $state(undefined);
  let lastResult: string | undefined = $state(undefined);
  let runError: string | undefined = $state(undefined);
  let copyOk = $state(false);
  let saveOk = $state(false);
  let saving = $state(false);
  let menuEl: HTMLDivElement | undefined = $state();

  // Reset transient feedback when the user dismisses or re-opens the menu.
  $effect(() => {
    if (!open) {
      lastResult = undefined;
      runError = undefined;
      copyOk = false;
      saveOk = false;
      pending = undefined;
    }
  });

  // Move focus into the dialog on open so screen readers announce the
  // role and so the Escape keydown handler below has somewhere reachable
  // to fire from. Without this the keyboard focus stays on whatever
  // triggered the menu and the dialog feels untethered.
  $effect(() => {
    if (open && menuEl) {
      menuEl.focus();
    }
  });

  const run = async (id: AiActionId): Promise<void> => {
    if (!target || !isTauri()) return;
    pending = id;
    runError = undefined;
    copyOk = false;
    saveOk = false;
    try {
      const result = await runAiAction(id, target.id);
      lastResult = result.text;
    } catch (err) {
      runError = err instanceof Error ? err.message : t.actionMenu.runFailed;
      lastResult = undefined;
    } finally {
      pending = undefined;
    }
  };

  const copyResult = async (): Promise<void> => {
    if (lastResult === undefined) return;
    try {
      await navigator.clipboard.writeText(lastResult);
      copyOk = true;
      // Reset feedback after a beat so repeated copies still flash visibly.
      setTimeout(() => (copyOk = false), 1500);
    } catch {
      copyOk = false;
    }
  };

  const saveResult = async (): Promise<void> => {
    if (lastResult === undefined || !isTauri()) return;
    saving = true;
    try {
      await saveAiResult(lastResult);
      saveOk = true;
      setTimeout(() => (saveOk = false), 1500);
    } catch {
      saveOk = false;
    } finally {
      saving = false;
    }
  };
</script>

{#if open}
  <div
    class="scrim"
    role="presentation"
    onclick={onClose}
    onkeydown={(e) => {
      if (e.key === "Escape") onClose();
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
        if (e.key === "Escape") {
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
        {#each ACTION_IDS as id (id)}
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
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
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

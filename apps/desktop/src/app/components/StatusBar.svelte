<script lang="ts">
  import { messages } from "../lib/i18n/index.svelte";
  import { aiEnabled, captureEnabled, settingsState } from "../stores/settings.svelte";

  type Props = {
    entryCount: number;
    elapsedMs: number | undefined;
    loading: boolean;
    errorMessage: string | undefined;
  };

  const { entryCount, elapsedMs, loading, errorMessage }: Props = $props();
  const t = $derived(messages());

  const capture = $derived(captureEnabled());
  const ai = $derived(aiEnabled());
  // Reference settingsState so the derived re-runs when settings reload.
  $effect(() => {
    void settingsState.loaded;
  });
</script>

<footer class="status">
  <span class="left">
    {#if errorMessage}
      <span class="error">{errorMessage}</span>
    {:else if loading}
      <span>{t.palette.searching}</span>
    {:else}
      <span>{t.status.entryCount(entryCount)}</span>
      {#if elapsedMs !== undefined}
        <span class="dot">·</span>
        <span>{t.palette.elapsed(elapsedMs)}</span>
      {/if}
    {/if}
  </span>
  <span class="right">
    <span class="badge" class:on={capture} class:off={!capture}>
      <span class="dot-icon" aria-hidden="true"></span>
      {capture ? t.status.captureOn : t.status.capturePaused}
    </span>
    <span class="badge" class:on={ai} class:off={!ai}>
      <span class="dot-icon" aria-hidden="true"></span>
      {ai ? t.status.aiOn : t.status.aiOff}
    </span>
    <span class="hints">
      <kbd>↑↓</kbd>{t.palette.hints.navigate}
      <kbd>Enter</kbd>{t.palette.hints.paste}
      <kbd>⌘K</kbd>{t.palette.hints.actions}
      <kbd>⌘,</kbd>{t.palette.hints.settings}
    </span>
  </span>
</footer>

<style>
  .status {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.4rem 1rem;
    border-top: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    background: var(--bg-elevated, rgba(255, 255, 255, 0.02));
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .left,
  .right {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .error {
    color: var(--danger, #f87171);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.05rem 0.45rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    border-radius: 999px;
  }
  .badge.on {
    border-color: rgba(120, 200, 140, 0.4);
    color: var(--ok, #86d29a);
  }
  .badge.off {
    color: var(--muted, rgba(255, 255, 255, 0.4));
  }
  .dot-icon {
    width: 0.4rem;
    height: 0.4rem;
    border-radius: 50%;
    background: currentColor;
  }
  .dot {
    opacity: 0.5;
  }
  .hints {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-left: 0.25rem;
  }
  kbd {
    padding: 0.05rem 0.35rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 4px;
    font-family: inherit;
    font-size: 0.7rem;
  }
</style>

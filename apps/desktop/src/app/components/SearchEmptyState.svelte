<script lang="ts">
  import { messages } from '../lib/i18n/index.svelte';

  type Props = {
    query: string;
    filtered: boolean;
    loading: boolean;
    errorMessage: string | undefined;
    capturePaused: boolean;
    onRetry: () => void;
    onClearSearch: () => void;
  };

  const { query, filtered, loading, errorMessage, capturePaused, onRetry, onClearSearch }: Props =
    $props();
  const t = $derived(messages());
  const searching = $derived(query.trim().length > 0);
  const message = $derived(
    loading
      ? t.palette.searching
      : (errorMessage ??
          (searching
            ? t.palette.emptyStates.noMatches
            : filtered
              ? t.palette.emptyStates.noFilterMatches
              : capturePaused
                ? t.palette.emptyStates.capturePaused
                : t.palette.emptyStates.start)),
  );
</script>

<div class="empty-state">
  <p role={errorMessage && !loading ? 'alert' : 'status'}>{message}</p>
  {#if !loading}
    {#if errorMessage}
      <button type="button" onclick={onRetry}>{t.palette.emptyStates.retry}</button>
    {:else if searching || filtered}
      <button type="button" onclick={onClearSearch}>{t.palette.emptyStates.clearSearch}</button>
    {/if}
  {/if}
</div>

<style>
  .empty-state {
    flex: 1;
    min-width: 0;
    padding: 1.5rem 1rem;
    color: var(--muted);
    text-align: center;
    font-size: 0.875rem;
  }
  p {
    margin: 0 0 0.75rem;
    overflow-wrap: anywhere;
  }
  button {
    padding: 0.4rem 0.65rem;
    border: 1px solid var(--border-strong);
    border-radius: 5px;
    background: transparent;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>

<script lang="ts">
  import { messages } from "../lib/i18n/index.svelte";
  import { type FilterPreset, filterState, setFilterPreset } from "../stores/searchFilters.svelte";
  import { runQuery, searchState } from "../stores/searchQuery.svelte";

  const presets: { key: FilterPreset; label: () => string }[] = [
    { key: "today", label: () => messages().palette.filters.today },
    { key: "last7days", label: () => messages().palette.filters.last7days },
    { key: "pinned", label: () => messages().palette.filters.pinned },
  ];

  const handleClick = async (preset: FilterPreset): Promise<void> => {
    setFilterPreset(preset);
    // Re-run the active query so the chip change takes effect right
    // away. Empty query falls through to refreshRecent which honours
    // the same filter set.
    await runQuery(searchState.query);
  };
</script>

<div class="filter-chips" role="toolbar" aria-label={messages().palette.filters.toolbarLabel}>
  {#each presets as { key, label } (key)}
    <button
      type="button"
      class="chip"
      class:active={filterState.preset === key}
      aria-pressed={filterState.preset === key}
      onclick={() => handleClick(key)}
    >
      {label()}
    </button>
  {/each}
</div>

<style>
  .filter-chips {
    display: flex;
    gap: 0.375rem;
    padding: 0.5rem 1rem;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    background: var(--bg-elevated, rgba(255, 255, 255, 0.02));
  }
  .chip {
    background: transparent;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    color: var(--muted, rgba(255, 255, 255, 0.55));
    padding: 0.2rem 0.65rem;
    border-radius: 999px;
    font-size: 0.78rem;
    font-family: inherit;
    cursor: pointer;
    transition:
      color 0.1s,
      border-color 0.1s,
      background 0.1s;
  }
  .chip:hover {
    color: var(--fg, #f5f5f5);
    border-color: var(--border-strong, rgba(255, 255, 255, 0.24));
  }
  .chip.active {
    color: var(--accent-fg, #14161a);
    background: var(--accent, #6c8dff);
    border-color: var(--accent, #6c8dff);
  }
  .chip:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 2px;
  }
</style>

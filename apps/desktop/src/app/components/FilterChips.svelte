<script lang="ts">
  import { messages } from '../lib/i18n/index.svelte';
  import type { ContentKind } from '../lib/types';
  import {
    type DatePreset,
    FILTERABLE_KINDS,
    filterState,
    setDatePreset,
    setSourceApp,
    toggleKind,
    togglePinnedOnly,
  } from '../stores/searchFilters.svelte';
  import { runQuery, searchState } from '../stores/searchQuery.svelte';

  const t = $derived(messages());

  const datePresets: { key: DatePreset; label: () => string }[] = [
    { key: 'today', label: () => t.palette.filters.today },
    { key: 'yesterday', label: () => t.palette.filters.yesterday },
    { key: 'last7days', label: () => t.palette.filters.last7days },
    { key: 'last30days', label: () => t.palette.filters.last30days },
  ];

  const kindLabel = (kind: ContentKind): string => {
    switch (kind) {
      case 'text':
        return t.palette.filters.kindText;
      case 'url':
        return t.palette.filters.kindUrl;
      case 'code':
        return t.palette.filters.kindCode;
      case 'image':
        return t.palette.filters.kindImage;
      case 'fileList':
        return t.palette.filters.kindFiles;
      default:
        return kind;
    }
  };

  // Source-app chips are derived from the apps present in the current results,
  // so the row adapts to what the user is actually looking at instead of a
  // fixed global list. The active selection is unioned in and kept first so it
  // survives the result set collapsing to a single app once applied. Capped so
  // a noisy result set can't overflow the chip area.
  const MAX_SOURCE_CHIPS = 6;
  const sourceApps = $derived.by((): string[] => {
    const seen = new Set<string>();
    const apps: string[] = [];
    const active = filterState.sourceApp;
    if (active !== undefined) {
      apps.push(active);
      seen.add(active);
    }
    for (const result of searchState.results) {
      const name = result.sourceAppName;
      if (name === undefined || seen.has(name)) continue;
      seen.add(name);
      apps.push(name);
      if (apps.length >= MAX_SOURCE_CHIPS) break;
    }
    return apps;
  });

  // Re-run the active query so a chip change takes effect right away. An empty
  // query falls through to refreshRecent, which honours the same filter set.
  const rerun = (): Promise<void> => runQuery(searchState.query);

  const onDate = async (key: DatePreset): Promise<void> => {
    setDatePreset(key);
    await rerun();
  };
  const onKind = async (kind: ContentKind): Promise<void> => {
    toggleKind(kind);
    await rerun();
  };
  const onSource = async (app: string): Promise<void> => {
    setSourceApp(app);
    await rerun();
  };
  const onPinned = async (): Promise<void> => {
    togglePinnedOnly();
    await rerun();
  };
</script>

<div class="filter-chips" role="toolbar" aria-label={t.palette.filters.toolbarLabel}>
  <div class="group" role="group" aria-label={t.palette.filters.dateGroup}>
    {#each datePresets as { key, label } (key)}
      <button
        type="button"
        class="chip"
        class:active={filterState.datePreset === key}
        aria-pressed={filterState.datePreset === key}
        onclick={() => onDate(key)}
      >
        {label()}
      </button>
    {/each}
  </div>

  <div class="group" role="group" aria-label={t.palette.filters.typeGroup}>
    {#each FILTERABLE_KINDS as kind (kind)}
      <button
        type="button"
        class="chip"
        class:active={filterState.kinds.includes(kind)}
        aria-pressed={filterState.kinds.includes(kind)}
        onclick={() => onKind(kind)}
      >
        {kindLabel(kind)}
      </button>
    {/each}
  </div>

  <button
    type="button"
    class="chip"
    class:active={filterState.pinnedOnly}
    aria-pressed={filterState.pinnedOnly}
    onclick={onPinned}
  >
    {t.palette.filters.pinned}
  </button>

  {#if sourceApps.length > 0}
    <div class="group" role="group" aria-label={t.palette.filters.sourceGroup}>
      {#each sourceApps as app (app)}
        <button
          type="button"
          class="chip source"
          class:active={filterState.sourceApp === app}
          aria-pressed={filterState.sourceApp === app}
          title={app}
          onclick={() => onSource(app)}
        >
          {app}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .filter-chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    /* Larger column gap separates groups; the tighter intra-group gap keeps
       each group reading as a unit without explicit divider rules. */
    gap: 0.4rem 0.85rem;
    padding: 0.5rem 1rem;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    background: var(--bg-elevated, rgba(255, 255, 255, 0.02));
  }
  .group {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 0.375rem;
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
  /* Source-app names can be long ("Visual Studio Code") and keep their own
     casing, unlike the short fixed-label chips. */
  .chip.source {
    max-width: 11rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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

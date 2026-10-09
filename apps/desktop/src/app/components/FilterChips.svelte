<script lang="ts">
  import { messages } from '../lib/i18n/index.svelte';
  import type { ContentKind } from '../lib/types';
  import { paletteSessionState, setFiltersRetained } from '../stores/paletteSession.svelte';
  import {
    clearFilters,
    type DatePreset,
    FILTERABLE_KINDS,
    filterState,
    hasActiveFilters,
    refreshSourceApps,
    setDatePreset,
    setSourceApp,
    sourceAppOptions,
    toggleKind,
    togglePinnedOnly,
  } from '../stores/searchFilters.svelte';
  import { runQuery, searchState } from '../stores/searchQuery.svelte';
  import FilterDropdown from './FilterDropdown.svelte';

  type Props = {
    // A narrow palette folds the date presets into one dropdown so the row
    // keeps to a single line, even with long translations.
    compactDates?: boolean;
  };

  const { compactDates = false }: Props = $props();

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

  // Source-app options for the dropdown: the backend's whole-history list
  // (`refreshSourceApps`), led by the active selection and followed by any app
  // on screen that the last fetch predates. Deduped; the backend caps the list.
  const sourceApps = $derived.by((): string[] => {
    const seen = new Set<string>();
    const apps: string[] = [];
    const push = (name: string | undefined): void => {
      if (name === undefined || seen.has(name)) return;
      seen.add(name);
      apps.push(name);
    };
    push(filterState.sourceApp);
    for (const app of sourceAppOptions.apps) push(app);
    for (const result of searchState.results) push(result.sourceAppName);
    return apps;
  });

  // Selection is folded into each dropdown's trigger label rather than shown as
  // extra chips: none → the axis name, one → that value, many → "<axis> <n>"
  // (a count, so no per-locale plural forms are needed).
  const typeLabel = $derived.by((): string => {
    const kinds = filterState.kinds;
    const [first] = kinds;
    if (first === undefined) return t.palette.filters.typeGroup;
    if (kinds.length === 1) return kindLabel(first);
    return `${t.palette.filters.typeGroup} ${kinds.length}`;
  });
  const appLabel = $derived(filterState.sourceApp ?? t.palette.filters.sourceShort);

  // Sentinel for the compact date menu's leading "Any time" reset row.
  const ANY_TIME = '__nagori_any_time__';
  const dateLabel = $derived(
    datePresets.find((preset) => preset.key === filterState.datePreset)?.label() ??
      t.palette.filters.dateGroup,
  );
  const dateItems = $derived([
    {
      value: ANY_TIME,
      label: t.palette.filters.anyTime,
      selected: filterState.datePreset === 'none',
      alwaysShown: true,
    },
    ...datePresets.map(({ key, label }) => ({
      value: key,
      label: label(),
      selected: filterState.datePreset === key,
    })),
  ]);

  const typeItems = $derived(
    FILTERABLE_KINDS.map((kind) => ({
      value: kind,
      label: kindLabel(kind),
      selected: filterState.kinds.includes(kind),
    })),
  );
  // Sentinel value for the leading "All apps" row. Picking it clears the
  // single-select source app — a discoverable alternative to the obscure
  // re-click-to-clear gesture. The double-underscore prefix keeps it from
  // colliding with a real app name. Only offered when there are apps to choose
  // between.
  const ALL_APPS = '__nagori_all_apps__';
  const appItems = $derived(
    sourceApps.length === 0
      ? []
      : [
          {
            value: ALL_APPS,
            label: t.palette.filters.allApps,
            selected: filterState.sourceApp === undefined,
            alwaysShown: true,
          },
          ...sourceApps.map((app) => ({
            value: app,
            label: app,
            selected: filterState.sourceApp === app,
          })),
        ],
  );

  // Re-run the active query so a filter change takes effect right away. An empty
  // query falls through to refreshRecent, which honours the same filter set.
  // Touching a filter also retires the "kept from last time" note: the set on
  // screen is now the user's current choice.
  const rerun = (): Promise<void> => {
    setFiltersRetained(false);
    return runQuery(searchState.query);
  };

  const onDate = async (key: DatePreset): Promise<void> => {
    setDatePreset(key);
    await rerun();
  };
  // The compact menu picks rather than toggles (radio semantics), so the
  // active preset stays on when its row is chosen again.
  const onDateMenu = async (value: string): Promise<void> => {
    const next = value === ANY_TIME ? 'none' : (value as DatePreset);
    if (next === filterState.datePreset) return;
    setDatePreset(next === 'none' ? filterState.datePreset : next);
    await rerun();
  };
  const onKind = async (value: string): Promise<void> => {
    toggleKind(value as ContentKind);
    await rerun();
  };
  const onSource = async (app: string): Promise<void> => {
    // The "All apps" sentinel clears the filter; any real app sets it.
    setSourceApp(app === ALL_APPS ? undefined : app);
    await rerun();
  };
  const onPinned = async (): Promise<void> => {
    togglePinnedOnly();
    await rerun();
  };
  const onClear = async (): Promise<void> => {
    clearFilters();
    await rerun();
  };
</script>

<div class="filter-chips" role="toolbar" aria-label={t.palette.filters.toolbarLabel}>
  {#if compactDates}
    <FilterDropdown
      label={dateLabel}
      active={filterState.datePreset !== 'none'}
      menuLabel={t.palette.filters.dateGroup}
      items={dateItems}
      multi={false}
      onSelect={onDateMenu}
    />
  {:else}
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
  {/if}

  <button
    type="button"
    class="chip"
    class:active={filterState.pinnedOnly}
    aria-pressed={filterState.pinnedOnly}
    onclick={onPinned}
  >
    {t.palette.filters.pinned}
  </button>

  <!-- High-cardinality axes (content kind, source app) collapse into dropdowns
       so the row stays one line instead of fanning out into a wall of chips. -->
  <div class="group menus" role="group" aria-label={t.palette.filters.typeGroup}>
    <FilterDropdown
      label={typeLabel}
      active={filterState.kinds.length > 0}
      menuLabel={t.palette.filters.typeGroup}
      items={typeItems}
      multi={true}
      onSelect={onKind}
    />
    <FilterDropdown
      label={appLabel}
      active={filterState.sourceApp !== undefined}
      menuLabel={t.palette.filters.sourceGroup}
      items={appItems}
      multi={false}
      onSelect={onSource}
      onOpen={() => void refreshSourceApps()}
      searchLabel={t.palette.filters.searchApps}
      noMatchesLabel={t.palette.filters.noAppMatches}
    />
  </div>

  {#if hasActiveFilters()}
    {#if paletteSessionState.filtersRetained}
      <span class="retained" role="status" data-testid="filters-retained"
        >{t.palette.filters.retained}</span
      >
    {/if}
    <button type="button" class="chip clear" aria-label={t.palette.filters.clear} onclick={onClear}>
      <span aria-hidden="true">✕</span>
    </button>
  {/if}
</div>

<style>
  .filter-chips {
    display: flex;
    /* One line, no wrapping: the dropdowns absorb the axes that used to make
       this row overflow, so chips no longer reflow into a second row. */
    flex-wrap: nowrap;
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
    flex-wrap: nowrap;
    gap: 0.375rem;
  }
  /* The "kept from last time" note sits just before the clear button, which it
     pushes to the far end of the row along with itself. */
  .retained {
    /* Yields first when a long translation fills the row: it shrinks to an
       ellipsis rather than pushing the chips out of the window. */
    flex: 0 1 auto;
    min-width: 0;
    margin-left: auto;
    overflow: hidden;
    color: var(--accent, #6c8dff);
    font-size: 0.72rem;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .retained + .clear {
    margin-left: 0;
  }
  /* The clear button is pushed to the far end of the row. */
  .clear {
    margin-left: auto;
    padding-inline: 0.5rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
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
    white-space: nowrap;
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

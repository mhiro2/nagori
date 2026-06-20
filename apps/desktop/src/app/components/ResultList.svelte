<script lang="ts">
  import { messages } from '../lib/i18n/index.svelte';
  import type { SearchResultDto } from '../lib/types';
  import ResultItem from './ResultItem.svelte';

  type Props = {
    items: SearchResultDto[];
    selectedIndex: number;
    // The query the current `items` were produced for (searchState.appliedQuery,
    // not the live keystroke query). Used to tell a brand-new search (scroll to
    // the top of the fresh list) apart from a same-query refresh such as a pin
    // toggle or delete (leave the scroll position untouched), and forwarded to
    // each row as the query its match highlight is computed against.
    appliedQuery?: string;
    onSelect: (index: number) => void;
    onConfirm: (index: number, event?: MouseEvent) => void;
    onTogglePin?: (index: number) => void;
    onContextMenu?: (index: number, event: MouseEvent) => void;
    multiSelected?: ReadonlySet<string>;
    emptyMessage?: string;
    // The action inspector owns the right column: the list becomes a reference
    // surface (selected row lifted, the rest receded) rather than a live,
    // hover-driven list. Purely visual — the palette gates hover selection.
    locked?: boolean;
  };

  const {
    items,
    selectedIndex,
    appliedQuery,
    onSelect,
    onConfirm,
    onTogglePin,
    onContextMenu,
    multiSelected,
    emptyMessage,
    locked = false,
  }: Props = $props();

  const effectiveEmpty = $derived(emptyMessage ?? messages().palette.empty);

  let listEl: HTMLDivElement | undefined = $state();

  // Remember what the previous effect run saw so this run can tell *why* it
  // fired — and only scroll when that reason warrants it:
  //   - a new query → jump to the top of the fresh result set;
  //   - pure keyboard navigation (same list array, cursor moved) → keep the
  //     moved cursor visible;
  //   - a same-query refresh (pin toggle, delete, clipboard capture) replaces
  //     the array without the user navigating → leave the scroll position put.
  // Driving this off the data (array identity + query) instead of a shared
  // suppression flag keeps it race-free: a concurrent refresh can't strand the
  // viewport, because each run decides purely from what it currently sees.
  let lastItems: SearchResultDto[] | undefined;
  let lastAppliedQuery: string | undefined;
  let lastSelectedIndex = -1;
  $effect(() => {
    const index = selectedIndex;
    const currentItems = items;
    const currentQuery = appliedQuery;
    const queryChanged = currentQuery !== lastAppliedQuery;
    const itemsReplaced = currentItems !== lastItems;
    const indexMoved = index !== lastSelectedIndex;
    lastItems = currentItems;
    lastAppliedQuery = currentQuery;
    lastSelectedIndex = index;
    if (!listEl) return;
    const shouldScroll = queryChanged || (!itemsReplaced && indexMoved);
    if (!shouldScroll) return;
    const nodes = listEl.querySelectorAll<HTMLElement>('.result-item');
    nodes[index]?.scrollIntoView({ block: 'nearest' });
  });
</script>

<div class="result-list" role="listbox" bind:this={listEl}>
  {#if items.length === 0}
    <p class="empty">{effectiveEmpty}</p>
  {:else}
    {#each items as item, index (item.id)}
      <ResultItem
        {item}
        {index}
        selected={index === selectedIndex}
        marked={multiSelected?.has(item.id) ?? false}
        query={appliedQuery}
        {locked}
        {onSelect}
        {onConfirm}
        {onTogglePin}
        {onContextMenu}
      />
    {/each}
  {/if}
</div>

<style>
  .result-list {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
    /* Cap visible rows by --palette-row-count when set on a parent (Palette).
       Each row is roughly 3rem tall (item padding + line-height); the cap
       lets the user shrink the palette without forcing every list to
       hard-code a height. */
    max-height: calc(var(--palette-row-count, 8) * 3rem);
  }
  .empty {
    padding: 1.5rem 1rem;
    color: var(--muted, rgba(255, 255, 255, 0.4));
    font-size: 0.875rem;
    text-align: center;
  }
</style>

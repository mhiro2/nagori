<script lang="ts">
  import { messages } from "../lib/i18n/index.svelte";
  import type { SearchResultDto } from "../lib/types";
  import ResultItem from "./ResultItem.svelte";

  type Props = {
    items: SearchResultDto[];
    selectedIndex: number;
    onSelect: (index: number) => void;
    onConfirm: (index: number, event?: MouseEvent) => void;
    multiSelected?: ReadonlySet<string>;
    emptyMessage?: string;
  };

  const { items, selectedIndex, onSelect, onConfirm, multiSelected, emptyMessage }: Props =
    $props();

  const effectiveEmpty = $derived(emptyMessage ?? messages().palette.empty);

  let listEl: HTMLDivElement | undefined = $state();

  $effect(() => {
    if (!listEl) return;
    const nodes = listEl.querySelectorAll<HTMLElement>(".result-item");
    nodes[selectedIndex]?.scrollIntoView({ block: "nearest" });
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
        {onSelect}
        {onConfirm}
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

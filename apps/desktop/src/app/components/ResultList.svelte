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
    // toggle or delete (leave the scroll position mostly untouched), and forwarded to
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
    // The list is a fixed-size page of the best matches. `canLoadMore` means
    // the page filled up and another one can be fetched; `limitReached` means
    // the backend cap was hit, so the footer suggests narrowing the search
    // instead of offering more rows.
    canLoadMore?: boolean;
    limitReached?: boolean;
    // More than one page was requested for this list. Keeps the footer (and
    // its live region) mounted when the last page comes back short, so the
    // final count is still announced.
    paged?: boolean;
    onLoadMore?: () => void;
    // DOM id of the listbox, referenced by the search combobox.
    listboxId?: string;
    // Forwarded to every row: see `ResultItem`'s `compact`.
    compact?: boolean;
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
    canLoadMore = false,
    limitReached = false,
    paged = false,
    onLoadMore,
    listboxId,
    compact = false,
  }: Props = $props();

  const t = $derived(messages());

  // Hover selects a row only when the pointer really moves over it. Engines
  // dispatch a synthetic mousemove at the old screen position when content
  // scrolls under a resting pointer (an arrow-key scroll, a capture landing at
  // the top), and acting on it would swap the keyboard's selection for
  // whatever row happens to sit under the cursor.
  let lastPointer: string | undefined;
  const handleHover = (index: number, event: MouseEvent): void => {
    const at = `${event.screenX},${event.screenY}`;
    if (at === lastPointer) return;
    lastPointer = at;
    if (index !== selectedIndex) onSelect(index);
  };

  const effectiveEmpty = $derived(emptyMessage ?? t.palette.empty);

  let listEl: HTMLDivElement | undefined = $state();

  // Remember what the previous effect run saw so this run can tell *why* it
  // fired — and only scroll when that reason warrants it:
  //   - a new query → jump to the top of the fresh result set;
  //   - keyboard navigation (cursor moved) → keep the moved cursor visible;
  //   - a same-query refresh (pin toggle, delete, clipboard capture) replaces
  //     the array but keeps the cursor on the selected entry → leave the
  //     scroll position put, unless the entry moved (a capture pushed it down,
  //     a pin floated it up), in which case scroll just far enough to keep the
  //     row the next Enter will act on in view.
  // Driving this off the data (cursor + query) instead of a shared
  // suppression flag keeps it race-free: a concurrent refresh can't strand the
  // viewport, because each run decides purely from what it currently sees.
  let lastAppliedQuery: string | undefined;
  let lastSelectedIndex = -1;
  $effect(() => {
    const index = selectedIndex;
    const currentQuery = appliedQuery;
    const queryChanged = currentQuery !== lastAppliedQuery;
    const indexMoved = index !== lastSelectedIndex;
    lastAppliedQuery = currentQuery;
    lastSelectedIndex = index;
    if (!listEl) return;
    const shouldScroll = queryChanged || indexMoved;
    if (!shouldScroll) return;
    const nodes = listEl.querySelectorAll<HTMLElement>('.result-item');
    nodes[index]?.scrollIntoView({ block: 'nearest' });
  });

  // When the last page arrives (the cap is reached or the page came back
  // short), the Show more button unmounts. If it held focus, the browser
  // would drop focus to the body and a following Enter would reach the
  // palette's confirm handler for whichever row was selected. Hand focus to the
  // first newly loaded row instead (focusing a row selects it), so the
  // keyboard user continues where the new rows start. New rows are found by id:
  // a larger limit re-ranks the list, so they are not necessarily at the end.
  let loadMoreFocused = false;
  let idsBeforeLoad: ReadonlySet<string> = new Set();
  const handleLoadMore = (): void => {
    idsBeforeLoad = new Set(items.map((item) => item.id));
    onLoadMore?.();
  };
  $effect(() => {
    if (canLoadMore || !loadMoreFocused || !listEl) return;
    loadMoreFocused = false;
    const active = document.activeElement;
    if (active !== null && active !== document.body) return;
    const rows = listEl.querySelectorAll<HTMLElement>('.result-item');
    const firstNew = items.findIndex((item) => !idsBeforeLoad.has(item.id));
    (rows[firstNew] ?? rows[selectedIndex])?.focus();
  });
</script>

<!-- The scroll container wraps the listbox so the page-limit footer can sit
     below the last row without becoming a non-option child of the listbox. -->
<div class="result-list" bind:this={listEl}>
  <!-- The highlighted row (the navigation cursor) is exposed through focus /
       the combobox's `aria-activedescendant`; `aria-selected` is reserved for
       the multi-selection, so the two never share one attribute. -->
  <div
    class="result-options"
    id={listboxId}
    role="listbox"
    aria-label={t.palette.resultsLabel}
    aria-multiselectable="true"
  >
    {#if items.length === 0}
      <p class="empty">{effectiveEmpty}</p>
    {:else}
      {#each items as item, index (item.id)}
        <ResultItem
          {item}
          {index}
          selected={index === selectedIndex}
          marked={multiSelected?.has(item.id) ?? false}
          multiActive={(multiSelected?.size ?? 0) > 0}
          {compact}
          query={appliedQuery}
          {locked}
          {onSelect}
          onHover={handleHover}
          {onConfirm}
          {onTogglePin}
          {onContextMenu}
        />
      {/each}
    {/if}
  </div>
  {#if items.length > 0 && (canLoadMore || limitReached || paged)}
    <div class="limit-notice">
      <!-- One persistent live region so each page's new count, and the switch
           to the cap notice, is announced. -->
      <span role="status">
        {canLoadMore
          ? t.palette.resultLimit.showing(items.length)
          : limitReached
            ? t.palette.resultLimit.reached(items.length)
            : t.palette.resultLimit.allShown(items.length)}
      </span>
      {#if canLoadMore}
        <button
          type="button"
          class="load-more"
          disabled={locked}
          onclick={handleLoadMore}
          onfocus={() => (loadMoreFocused = true)}
          onblur={(event) => {
            // Removing the focused button may fire a blur with no target;
            // only a real move elsewhere clears the flag.
            if (event.relatedTarget !== null) loadMoreFocused = false;
          }}
        >
          {t.palette.resultLimit.showMore}
        </button>
      {/if}
    </div>
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
  .limit-notice {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: 0.25rem 0.75rem;
    padding: 0.75rem 1rem;
    color: var(--muted);
    font-size: 0.75rem;
    text-align: center;
  }
  .load-more {
    padding: 0.25rem 0.65rem;
    border: 1px solid var(--border-strong);
    border-radius: 5px;
    background: transparent;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .load-more:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .load-more:disabled {
    cursor: default;
    opacity: 0.5;
  }
  .empty {
    padding: 1.5rem 1rem;
    color: var(--muted, rgba(255, 255, 255, 0.4));
    font-size: 0.875rem;
    text-align: center;
  }
</style>

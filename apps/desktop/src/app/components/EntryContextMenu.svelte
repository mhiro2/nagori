<script lang="ts">
  import { onMount } from 'svelte';

  import { messages } from '../lib/i18n/index.svelte';
  import { closeEntryContextMenu, entryContextMenuState } from '../stores/entryContextMenu.svelte';
  import {
    copyEntriesByIds,
    copyEntryById,
    deleteEntriesByIds,
    deleteEntryById,
    openPasteFormatPickerFor,
    pasteEntryById,
    togglePinEntry,
  } from '../stores/searchActions';

  type Props = {
    // Opening the action inspector docks a right-panel that lives in `Palette`
    // state, so the inspector row hands back to the parent rather than driving
    // it from here. The id is the captured single target.
    onOpenActions: (id: string) => void;
  };

  const { onOpenActions }: Props = $props();

  const t = $derived(messages());

  type MenuRow =
    | { kind: 'separator'; key: string }
    | { kind: 'item'; key: string; label: string; run: () => void };

  // The menu acts on the id(s) captured when it opened (see the store), never
  // on the live selection. Multiple ids only ever occur when the right-clicked
  // row was part of the multi-selection, so the multi rows mirror exactly what
  // a multi Enter / click already supports today: combined copy and bulk
  // delete. A single target gets the full per-entry surface.
  const rows = $derived.by((): MenuRow[] => {
    // Snapshot the captured ids into the row closures so they survive the
    // store being cleared on close and never re-read the live selection.
    const ids = [...entryContextMenuState.targetIds];
    if (ids.length === 0) return [];
    if (ids.length > 1) {
      return [
        {
          kind: 'item',
          key: 'copy',
          label: t.contextMenu.copy,
          run: () => void copyEntriesByIds(ids),
        },
        { kind: 'separator', key: 'sep-multi' },
        {
          kind: 'item',
          key: 'delete',
          label: t.contextMenu.delete,
          run: () => void deleteEntriesByIds(ids),
        },
      ];
    }
    const id = ids[0]!;
    const pinned = entryContextMenuState.primaryPinned;
    const single: MenuRow[] = [
      {
        kind: 'item',
        key: 'paste',
        label: t.contextMenu.paste,
        run: () => void pasteEntryById(id),
      },
      { kind: 'item', key: 'copy', label: t.contextMenu.copy, run: () => void copyEntryById(id) },
    ];
    // Only offer "paste as…" when the entry can actually be pasted more than one
    // way; otherwise it would either duplicate plain Paste or (worse) try a
    // format the entry can't produce. When shown, it always opens the picker.
    if (entryContextMenuState.offersFormatChoice) {
      single.push({
        kind: 'item',
        key: 'pasteAs',
        label: t.contextMenu.pasteAs,
        run: () => void openPasteFormatPickerFor(id),
      });
    }
    single.push(
      {
        kind: 'item',
        key: 'pin',
        label: pinned ? t.contextMenu.unpin : t.contextMenu.pin,
        run: () => void togglePinEntry({ id, pinned }),
      },
      { kind: 'separator', key: 'sep-actions' },
      { kind: 'item', key: 'actions', label: t.contextMenu.actions, run: () => onOpenActions(id) },
      { kind: 'separator', key: 'sep-delete' },
      {
        kind: 'item',
        key: 'delete',
        label: t.contextMenu.delete,
        run: () => void deleteEntryById(id),
      },
    );
    return single;
  });

  let panelEl: HTMLDivElement | undefined = $state();

  // Start at the raw click point; the clamp effect corrects this the moment the
  // panel has a measurable size so the menu never spills past the small,
  // fixed-size palette window.
  let left = $state(entryContextMenuState.x);
  let top = $state(entryContextMenuState.y);
  const EDGE_MARGIN = 6;
  $effect(() => {
    const x = entryContextMenuState.x;
    const y = entryContextMenuState.y;
    const el = panelEl;
    if (!el) {
      left = x;
      top = y;
      return;
    }
    const maxLeft = window.innerWidth - el.offsetWidth - EDGE_MARGIN;
    const maxTop = window.innerHeight - el.offsetHeight - EDGE_MARGIN;
    left = Math.max(EDGE_MARGIN, Math.min(x, maxLeft));
    top = Math.max(EDGE_MARGIN, Math.min(y, maxTop));
  });

  const itemButtons = (): HTMLButtonElement[] =>
    panelEl ? Array.from(panelEl.querySelectorAll<HTMLButtonElement>('.item')) : [];

  const focusItem = (index: number): void => {
    const els = itemButtons();
    if (els.length === 0) return;
    const wrapped = ((index % els.length) + els.length) % els.length;
    els[wrapped]?.focus();
  };

  // Land focus on the menu the instant it mounts so it owns the keyboard:
  // Escape then routes into `onKeydown` (which swallows it) instead of bubbling
  // to the window handlers, where App.svelte's Escape listener would otherwise
  // hide the whole palette. Focus synchronously in the mount effect — a
  // deferred focus loses the race to the search input in the webview. Mirrors
  // PasteFormatPicker. Tracking `targetIds` also re-focuses the first item when
  // an outside right-click re-targets the menu to another row (the row set, and
  // so the buttons, can change), keeping the keyboard owned across the move.
  $effect(() => {
    void entryContextMenuState.targetIds;
    focusItem(0);
  });

  // All navigation is handled and swallowed here. The palette routes
  // arrows / Enter / Escape at the window level, so an un-stopped keystroke
  // would leak out and move the result selection, paste, or hide the palette.
  const onKeydown = (event: KeyboardEvent): void => {
    const els = itemButtons();
    const current = els.findIndex((el) => el === document.activeElement);
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        event.stopPropagation();
        focusItem(current + 1);
        break;
      case 'ArrowUp':
        event.preventDefault();
        event.stopPropagation();
        focusItem(current - 1);
        break;
      case 'Home':
        event.preventDefault();
        event.stopPropagation();
        focusItem(0);
        break;
      case 'End':
        event.preventDefault();
        event.stopPropagation();
        focusItem(els.length - 1);
        break;
      case 'Escape':
        event.preventDefault();
        event.stopPropagation();
        closeEntryContextMenu();
        break;
      default:
        // Swallow everything else too (Enter/Space activate the focused row via
        // the button's own click) so a remapped palette chord can't fire while
        // the menu owns the keyboard.
        event.stopPropagation();
    }
  };

  const runRow = (run: () => void): void => {
    // The row closures captured their target id by value when `rows` was
    // derived, so closing the menu first (which clears the store) is safe.
    run();
    closeEntryContextMenu();
  };

  // Dismissal lives on document-level capture listeners rather than a backdrop
  // element: a backdrop would swallow the right-click that should move the menu
  // to another row. Capture runs before the row's own bubble handler, so an
  // outside right-click closes this menu and the row handler immediately
  // re-opens it at the new spot. Inside clicks are ignored so item buttons
  // still activate.
  onMount(() => {
    const onDocPointerDown = (event: PointerEvent): void => {
      if (panelEl && event.target instanceof Node && panelEl.contains(event.target)) return;
      closeEntryContextMenu();
    };
    const onDocContextMenu = (event: MouseEvent): void => {
      // Suppress the native webview menu for as long as ours is open.
      event.preventDefault();
      if (panelEl && event.target instanceof Node && panelEl.contains(event.target)) return;
      closeEntryContextMenu();
    };
    document.addEventListener('pointerdown', onDocPointerDown, true);
    document.addEventListener('contextmenu', onDocContextMenu, true);
    return () => {
      document.removeEventListener('pointerdown', onDocPointerDown, true);
      document.removeEventListener('contextmenu', onDocContextMenu, true);
    };
  });
</script>

<div
  bind:this={panelEl}
  class="context-menu"
  role="menu"
  aria-label={t.contextMenu.label}
  tabindex="-1"
  style="left: {left}px; top: {top}px;"
  onkeydown={onKeydown}
>
  {#each rows as row (row.key)}
    {#if row.kind === 'separator'}
      <div class="separator" role="separator"></div>
    {:else}
      <button
        type="button"
        class="item"
        role="menuitem"
        data-testid={`context-menu-${row.key}`}
        onclick={() => runRow(row.run)}>{row.label}</button
      >
    {/if}
  {/each}
</div>

<style>
  .context-menu {
    position: fixed;
    z-index: 40;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 11rem;
    max-width: 18rem;
    padding: 0.3rem;
    /* Opaque popover surface so the result list never bleeds through. */
    background: var(--bg-overlay, #1d1f23);
    border: 1px solid var(--border-strong, rgba(255, 255, 255, 0.24));
    border-radius: 0.5rem;
    box-shadow: 0 0.6rem 1.6rem rgba(0, 0, 0, 0.4);
  }
  .context-menu:focus {
    outline: none;
  }
  .item {
    width: 100%;
    padding: 0.4rem 0.6rem;
    border: none;
    border-radius: 0.35rem;
    background: transparent;
    color: var(--fg, #f5f5f5);
    font: inherit;
    font-size: 0.82rem;
    text-align: left;
    cursor: pointer;
    transition:
      background 0.1s,
      color 0.1s;
  }
  .item:hover {
    background: color-mix(in srgb, var(--fg, #f5f5f5) 8%, transparent);
  }
  .item:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: -2px;
  }
  .separator {
    height: 1px;
    margin: 0.2rem 0.3rem;
    background: var(--border, rgba(255, 255, 255, 0.16));
  }
</style>

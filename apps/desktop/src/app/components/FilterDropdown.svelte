<script lang="ts">
  import { tick } from 'svelte';

  type DropdownItem = {
    value: string;
    // Already-localized text shown in the menu row.
    label: string;
    selected: boolean;
  };

  type Props = {
    // Trigger text. The parent summarizes selection into this (e.g. "Type",
    // "URL", or "Type 2") so the dropdown stays presentation-only.
    label: string;
    // Drives the trigger's active styling — true when any value is selected.
    active: boolean;
    // aria-label for both the trigger and the menu, and the menu's heading.
    menuLabel: string;
    items: DropdownItem[];
    // `true` → checkbox semantics (stays open for more toggles); `false` →
    // radio semantics (selecting commits and closes).
    multi: boolean;
    onSelect: (value: string) => void;
  };

  const { label, active, menuLabel, items, multi, onSelect }: Props = $props();

  // The visible trigger label folds in the current selection (e.g. "URL",
  // "Type 2"), so the accessible name must too — a bare `menuLabel` would let
  // a screen reader hear the axis but never the active value. Only append the
  // value once something is selected, so the idle trigger stays just "Type".
  const triggerAria = $derived(active ? `${menuLabel}: ${label}` : menuLabel);

  let open = $state(false);
  let wrapperEl: HTMLDivElement | undefined = $state();
  let triggerEl: HTMLButtonElement | undefined = $state();
  let panelEl: HTMLDivElement | undefined = $state();

  // Query the rendered rows on demand rather than threading an array of binds
  // through the `{#each}` — focus moves by DOM order, which is exactly what
  // these queries return.
  const itemButtons = (): HTMLButtonElement[] =>
    panelEl ? Array.from(panelEl.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]')) : [];

  const focusItem = (index: number): void => {
    const els = itemButtons();
    if (els.length === 0) return;
    const wrapped = ((index % els.length) + els.length) % els.length;
    els[wrapped]?.focus();
  };

  const openMenu = async (): Promise<void> => {
    if (items.length === 0) return;
    open = true;
    await tick();
    // Land on the current choice when re-opening so the highlighted row
    // reflects state; otherwise start at the top.
    const selectedIdx = items.findIndex((it) => it.selected);
    focusItem(selectedIdx >= 0 ? selectedIdx : 0);
  };

  const closeMenu = (returnFocus = true): void => {
    if (!open) return;
    open = false;
    if (returnFocus) triggerEl?.focus();
  };

  const onTriggerKeydown = (event: KeyboardEvent): void => {
    if (event.key === 'ArrowDown' || event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      // Stop the palette's window-level handler from also acting on this key
      // (ArrowDown would move the result selection, Enter would paste).
      event.stopPropagation();
      void openMenu();
    }
  };

  const onTriggerClick = (): void => {
    if (open) closeMenu(false);
    else void openMenu();
  };

  const activate = (value: string): void => {
    onSelect(value);
    // A single-select pick reads as a commit, so close. Multi-select keeps the
    // menu open so several kinds can be toggled in one visit.
    if (!multi) closeMenu();
  };

  // All menu navigation is handled and swallowed here: the palette routes
  // arrows / Enter / Escape at the window level, so an un-stopped keystroke
  // would leak out and move the result selection or dismiss the whole palette.
  // Mirrors ActionInspector, which likewise stops its own keydowns.
  const onPanelKeydown = (event: KeyboardEvent): void => {
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
        closeMenu();
        break;
      case 'Tab':
        // Don't leave an orphaned open menu, and don't let Tab reach the
        // window handler — a user can remap a palette action onto Tab, which
        // would otherwise fire as focus leaves the menu. No preventDefault, so
        // focus still moves on naturally.
        event.stopPropagation();
        closeMenu(false);
        break;
    }
  };

  const onItemKeydown = (event: KeyboardEvent, value: string): void => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      event.stopPropagation();
      activate(value);
    }
  };

  // Dismiss on a click outside the wrapper. pointerdown (capture) so it lands
  // before a click on a sibling trigger re-opens, and the wrapper test keeps
  // clicks inside the panel from self-closing.
  $effect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent): void => {
      if (wrapperEl && !wrapperEl.contains(event.target as Node)) closeMenu(false);
    };
    window.addEventListener('pointerdown', onPointerDown, true);
    return () => window.removeEventListener('pointerdown', onPointerDown, true);
  });
</script>

<div class="dropdown" bind:this={wrapperEl}>
  <button
    bind:this={triggerEl}
    type="button"
    class="chip trigger"
    class:active
    aria-haspopup="menu"
    aria-expanded={open}
    aria-label={triggerAria}
    disabled={items.length === 0}
    onclick={onTriggerClick}
    onkeydown={onTriggerKeydown}
  >
    <span class="trigger-label">{label}</span>
    <span class="caret" aria-hidden="true"></span>
  </button>

  {#if open}
    <div
      bind:this={panelEl}
      class="panel"
      role="menu"
      aria-label={menuLabel}
      tabindex="-1"
      onkeydown={onPanelKeydown}
    >
      {#each items as item (item.value)}
        <button
          type="button"
          class="item"
          class:selected={item.selected}
          role={multi ? 'menuitemcheckbox' : 'menuitemradio'}
          aria-checked={item.selected}
          title={item.label}
          onclick={() => activate(item.value)}
          onkeydown={(event) => onItemKeydown(event, item.value)}
        >
          <!-- Square (checkbox) for multi-select, circle (radio) for single:
               the universal "pick several" vs "pick one" cue. Drawn in CSS,
               not glyphs, so it renders crisply across platform fonts. -->
          <span
            class="indicator"
            class:checkbox={multi}
            class:radio={!multi}
            class:on={item.selected}
            aria-hidden="true"
          ></span>
          <span class="item-label">{item.label}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .dropdown {
    position: relative;
    display: inline-flex;
  }
  /* Mirror FilterChips' `.chip` so the trigger reads as part of the same row,
     with room for the trailing caret. */
  .trigger {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    background: transparent;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    color: var(--muted, rgba(255, 255, 255, 0.55));
    padding: 0.2rem 0.55rem 0.2rem 0.65rem;
    border-radius: 999px;
    font-size: 0.78rem;
    font-family: inherit;
    cursor: pointer;
    transition:
      color 0.1s,
      border-color 0.1s,
      background 0.1s;
  }
  .trigger:hover:not(:disabled),
  .trigger[aria-expanded='true'] {
    color: var(--fg, #f5f5f5);
    border-color: var(--border-strong, rgba(255, 255, 255, 0.24));
  }
  .trigger.active {
    color: var(--accent-fg, #14161a);
    background: var(--accent, #6c8dff);
    border-color: var(--accent, #6c8dff);
  }
  .trigger:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .trigger:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 2px;
  }
  .trigger-label {
    max-width: 9rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* A CSS-drawn triangle rather than a ▾ glyph: the glyph renders
     inconsistently small across platform fonts (notably macOS). A border
     triangle is crisp, sized in rem, and inherits the trigger color via
     currentColor so it tracks the hover / active states. */
  .caret {
    flex: none;
    width: 0;
    height: 0;
    border-left: 0.3rem solid transparent;
    border-right: 0.3rem solid transparent;
    border-top: 0.34rem solid currentColor;
    opacity: 0.85;
  }
  .panel {
    position: absolute;
    top: calc(100% + 0.3rem);
    left: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 9rem;
    max-width: 16rem;
    max-height: 16rem;
    overflow-y: auto;
    padding: 0.25rem;
    /* Solid surface — the panel overlays the result list, so a translucent
       --bg-elevated would let rows bleed through. --bg-overlay is the opaque
       popover token (dark #1d1f23 / light #ffffff). */
    background: var(--bg-overlay, #1d1f23);
    border: 1px solid var(--border-strong, rgba(255, 255, 255, 0.24));
    border-radius: 0.5rem;
    box-shadow: 0 0.5rem 1.5rem rgba(0, 0, 0, 0.35);
  }
  .item {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    padding: 0.32rem 0.5rem;
    border: none;
    border-radius: 0.35rem;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font: inherit;
    font-size: 0.8rem;
    text-align: left;
    cursor: pointer;
    transition:
      background 0.1s,
      color 0.1s;
  }
  .item:hover {
    background: color-mix(in srgb, var(--fg, #f5f5f5) 8%, transparent);
    color: var(--fg, #f5f5f5);
  }
  .item.selected {
    color: var(--fg, #f5f5f5);
  }
  .item:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: -2px;
  }
  /* Shared box: 14px, with a border that the on-state fills. `.checkbox`
     squares it off; `.radio` rounds it to a circle. */
  .indicator {
    flex: none;
    box-sizing: border-box;
    position: relative;
    width: 0.875rem;
    height: 0.875rem;
    border: 1.5px solid var(--border-strong, rgba(255, 255, 255, 0.24));
    transition:
      background 0.1s,
      border-color 0.1s;
  }
  .indicator.checkbox {
    border-radius: 0.2rem;
  }
  .indicator.radio {
    border-radius: 50%;
  }
  .item:hover .indicator {
    border-color: var(--border-hover, rgba(255, 255, 255, 0.32));
  }
  /* Checked checkbox: filled accent box with a centered SVG tick. An SVG
     (positioned `center`) can't drift off-axis the way a rotated-border tick
     does; its stroke is --accent-fg (#14161a), matching .chip.active's
     text-on-accent colour in both themes. */
  .indicator.checkbox.on {
    background-color: var(--accent, #6c8dff);
    border-color: var(--accent, #6c8dff);
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Cpath d='M3.5 8.5l3 3 6-6.5' fill='none' stroke='%2314161a' stroke-width='2.2' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: center;
    background-size: 0.75rem;
  }
  /* Selected radio: accent ring with a filled centre dot (box stays unfilled,
     so it reads as a radio rather than a checkbox). */
  .indicator.radio.on {
    border-color: var(--accent, #6c8dff);
  }
  .indicator.radio.on::after {
    content: '';
    position: absolute;
    inset: 0.18rem;
    border-radius: 50%;
    background: var(--accent, #6c8dff);
  }
  .item-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>

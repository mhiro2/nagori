<script lang="ts">
  import { tick } from 'svelte';

  import { messages } from '../lib/i18n/index.svelte';
  import type { PasteOption } from '../lib/types';
  import { pasteFormatPickerState } from '../stores/pasteFormatPicker.svelte';
  import { cancelPasteFormat, confirmPasteFormat } from '../stores/searchActions';

  const t = $derived(messages());

  // One row per choice: a leading "keep original" (the default Preserve paste)
  // followed by each pasteable representation in canonical order. `option`
  // is `undefined` for the original row.
  type Row = { key: string; label: string; option: PasteOption | undefined };

  // Image is the only category that can repeat (PNG + JPEG, say), so append
  // the concrete subtype there to keep the rows distinguishable; every other
  // category maps to a single MIME and reads cleanly on its own.
  const labelFor = (option: PasteOption): string => {
    const category = t.pastePicker.categories[option.category];
    if (option.category !== 'image') return category;
    const subtype = option.mime.split('/')[1]?.toUpperCase();
    return subtype ? `${category} (${subtype})` : category;
  };

  const rows = $derived<Row[]>([
    { key: 'original', label: t.pastePicker.keepOriginal, option: undefined },
    ...pasteFormatPickerState.options.map((option) => ({
      key: option.mime,
      label: labelFor(option),
      option,
    })),
  ]);

  let panelEl: HTMLDivElement | undefined = $state();

  const rowButtons = (): HTMLButtonElement[] =>
    panelEl ? Array.from(panelEl.querySelectorAll<HTMLButtonElement>('.row')) : [];

  const focusRow = (index: number): void => {
    const els = rowButtons();
    if (els.length === 0) return;
    const wrapped = ((index % els.length) + els.length) % els.length;
    els[wrapped]?.focus();
  };

  // Land focus on the first row when the picker opens so the keyboard owns it
  // immediately (and Escape routes into `onKeydown`, not the palette below).
  $effect(() => {
    if (panelEl) void tick().then(() => focusRow(0));
  });

  // All navigation is handled and swallowed here: the palette routes
  // arrows / Enter / Escape at the window level, so an un-stopped keystroke
  // would leak out and move the result selection or paste the entry. Mirrors
  // FilterDropdown / ActionInspector, which likewise stop their own keydowns.
  const onKeydown = (event: KeyboardEvent): void => {
    const els = rowButtons();
    const current = els.findIndex((el) => el === document.activeElement);
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        event.stopPropagation();
        focusRow(current + 1);
        break;
      case 'ArrowUp':
        event.preventDefault();
        event.stopPropagation();
        focusRow(current - 1);
        break;
      case 'Home':
        event.preventDefault();
        event.stopPropagation();
        focusRow(0);
        break;
      case 'End':
        event.preventDefault();
        event.stopPropagation();
        focusRow(els.length - 1);
        break;
      case 'Escape':
        event.preventDefault();
        event.stopPropagation();
        cancelPasteFormat();
        break;
      default:
        // Swallow everything else too (Enter/Space activate the focused row
        // via the button's own click) so a remapped palette chord can't fire
        // while the picker owns the keyboard.
        event.stopPropagation();
    }
  };
</script>

<!-- A small modal picker centered over the palette body. `aria-modal` marks it
     as owning input for its lifetime: it traps no focus programmatically but
     swallows its keydowns and dismisses on a backdrop click, so the palette
     beneath stays inert until a choice is made or it is cancelled. -->
<div
  class="backdrop"
  role="presentation"
  onpointerdown={(event) => {
    if (event.target === event.currentTarget) cancelPasteFormat();
  }}
>
  <div
    bind:this={panelEl}
    class="picker"
    role="dialog"
    aria-modal="true"
    aria-label={t.pastePicker.title}
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <p class="title" id="paste-picker-title">{t.pastePicker.title}</p>
    <div class="rows" role="menu" aria-labelledby="paste-picker-title">
      {#each rows as row (row.key)}
        <button
          type="button"
          class="row"
          role="menuitem"
          data-testid={`paste-format-${row.key}`}
          onclick={() => void confirmPasteFormat(row.option)}
        >
          {row.label}
        </button>
      {/each}
    </div>
  </div>
</div>

<style>
  .backdrop {
    /* Fixed so it covers the whole palette webview (search box + body)
       regardless of any positioned ancestor — a click anywhere outside the
       panel cancels, and the inert palette beneath can't be re-targeted. */
    position: fixed;
    inset: 0;
    z-index: 30;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--bg, #14161a) 45%, transparent);
  }
  .picker {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    min-width: 14rem;
    max-width: 22rem;
    max-height: 80%;
    overflow-y: auto;
    padding: 0.75rem;
    /* Opaque popover surface so the result list never bleeds through. */
    background: var(--bg-overlay, #1d1f23);
    border: 1px solid var(--border-strong, rgba(255, 255, 255, 0.24));
    border-radius: 0.6rem;
    box-shadow: 0 0.75rem 2rem rgba(0, 0, 0, 0.4);
  }
  .picker:focus {
    outline: none;
  }
  .title {
    margin: 0 0 0.1rem;
    padding: 0 0.3rem;
    font-size: 0.8125rem;
    font-weight: 600;
    color: var(--fg, #f5f5f5);
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .row {
    width: 100%;
    padding: 0.4rem 0.55rem;
    border: none;
    border-radius: 0.4rem;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.7));
    font: inherit;
    font-size: 0.82rem;
    text-align: left;
    cursor: pointer;
    transition:
      background 0.1s,
      color 0.1s;
  }
  .row:hover {
    background: color-mix(in srgb, var(--fg, #f5f5f5) 8%, transparent);
    color: var(--fg, #f5f5f5);
  }
  .row:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: -2px;
    color: var(--fg, #f5f5f5);
  }
</style>

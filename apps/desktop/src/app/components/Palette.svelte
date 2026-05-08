<script lang="ts">
  import { buildBindings, resolveAction } from "../lib/keybindings";
  import { closePalette } from "../lib/commands";
  import { isTauri } from "../lib/tauri";
  import {
    confirmSelection,
    confirmSelectionWithAlternateFormat,
    copyMultiSelection,
    copySelection,
    deleteMultiSelection,
    deleteSelection,
    togglePinSelection,
  } from "../stores/searchActions";
  import {
    clearMultiSelect,
    multiSelectState,
    rangeSelectMulti,
    selectAllMulti,
    toggleMultiSelect,
  } from "../stores/searchMultiSelect.svelte";
  import { hydratePreview, previewState } from "../stores/searchPreview.svelte";
  import { refreshRecent, scheduleQuery, searchState } from "../stores/searchQuery.svelte";
  import {
    currentSelection,
    selectByIndex,
    selectFirst,
    selectLast,
    selectNext,
    selectPrev,
  } from "../stores/searchSelection";
  import { refreshSettings, settingsState } from "../stores/settings.svelte";
  import { showSettings } from "../stores/view.svelte";
  import ActionMenu from "./ActionMenu.svelte";
  import FilterChips from "./FilterChips.svelte";
  import OnboardingBanner from "./OnboardingBanner.svelte";
  import PreviewPane from "./PreviewPane.svelte";
  import ResultList from "./ResultList.svelte";
  import SearchBox from "./SearchBox.svelte";
  import StatusBar from "./StatusBar.svelte";

  let actionMenuOpen = $state(false);

  $effect(() => {
    void Promise.all([refreshRecent(), refreshSettings()]);
  });

  const selected = $derived(currentSelection());
  const resultIds = $derived(searchState.results.map((r) => r.id));

  // Debounce so rapid arrow-key navigation across a 50-row list doesn't fire
  // a `get_entry_preview` IPC per row. Only the row the user settles on
  // crosses the bridge.
  const PREVIEW_DEBOUNCE_MS = 60;
  let previewDebounceTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const id = selected?.id;
    if (previewDebounceTimer !== undefined) clearTimeout(previewDebounceTimer);
    previewDebounceTimer = setTimeout(() => {
      previewDebounceTimer = undefined;
      void hydratePreview(id);
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      if (previewDebounceTimer !== undefined) {
        clearTimeout(previewDebounceTimer);
        previewDebounceTimer = undefined;
      }
    };
  });

  const handleInput = (next: string): void => {
    scheduleQuery(next);
  };

  const handleConfirm = (index: number, event?: MouseEvent): void => {
    selectByIndex(index);
    const id = searchState.results[index]?.id;
    if (id !== undefined) {
      if (event?.metaKey) {
        toggleMultiSelect(id);
        return;
      }
      if (event?.shiftKey) {
        rangeSelectMulti(resultIds, id);
        return;
      }
    }
    if (multiSelectState.selected.size > 0) {
      void copyMultiSelection();
      return;
    }
    void confirmSelection();
  };

  const handleSelect = (index: number): void => {
    selectByIndex(index);
  };

  const showPreviewPane = $derived(settingsState.settings?.showPreviewPane ?? true);
  const paletteRowCount = $derived(settingsState.settings?.paletteRowCount ?? 8);
  const paletteBindings = $derived(buildBindings(settingsState.settings?.paletteHotkeys ?? {}));
  let previewExpanded = $state(false);

  const handleKeydown = (event: KeyboardEvent): void => {
    const action = resolveAction(event, paletteBindings);
    if (!action) return;
    event.preventDefault();
    switch (action) {
      case "select-next":
        selectNext();
        break;
      case "select-prev":
        selectPrev();
        break;
      case "select-first":
        selectFirst();
        break;
      case "select-last":
        selectLast();
        break;
      case "confirm":
        if (multiSelectState.selected.size > 0) void copyMultiSelection();
        else void confirmSelection();
        break;
      case "confirm-alternate-format":
        if (multiSelectState.selected.size > 0) void copyMultiSelection();
        else void confirmSelectionWithAlternateFormat();
        break;
      case "copy":
        if (multiSelectState.selected.size > 0) void copyMultiSelection();
        else void copySelection();
        break;
      case "open-actions":
        actionMenuOpen = true;
        break;
      case "toggle-pin":
        void togglePinSelection();
        break;
      case "delete":
        if (multiSelectState.selected.size > 0) void deleteMultiSelection();
        else void deleteSelection();
        break;
      case "clear-query":
        scheduleQuery("");
        break;
      case "open-preview":
        previewExpanded = !previewExpanded;
        break;
      case "open-settings":
        showSettings();
        break;
      case "multi-toggle": {
        const id = currentSelection()?.id;
        if (id !== undefined) toggleMultiSelect(id);
        break;
      }
      case "multi-select-all":
        selectAllMulti(resultIds);
        break;
      case "close":
        if (actionMenuOpen) actionMenuOpen = false;
        else if (multiSelectState.selected.size > 0) clearMultiSelect();
        else if (previewExpanded) previewExpanded = false;
        else if (isTauri()) void closePalette();
        break;
    }
  };
</script>

<section class="palette" style="--palette-row-count: {paletteRowCount}">
  <SearchBox value={searchState.query} onInput={handleInput} onKeydown={handleKeydown} />
  <FilterChips />
  <OnboardingBanner />
  <div class="body" class:single-column={!showPreviewPane && !previewExpanded} class:preview-only={previewExpanded}>
    {#if !previewExpanded}
      <ResultList
        items={searchState.results}
        selectedIndex={searchState.selectedIndex}
        multiSelected={multiSelectState.selected}
        onSelect={handleSelect}
        onConfirm={handleConfirm}
      />
    {/if}
    {#if showPreviewPane || previewExpanded}
      <PreviewPane item={selected} preview={previewState.preview} loading={previewState.loading} errorMessage={previewState.errorMessage} expanded={previewExpanded} />
    {/if}
  </div>
  <StatusBar
    entryCount={searchState.results.length}
    elapsedMs={searchState.lastElapsedMs}
    loading={searchState.loading}
    errorMessage={searchState.errorMessage ?? settingsState.errorMessage}
    selectedCount={multiSelectState.selected.size}
  />
</section>

<ActionMenu
  open={actionMenuOpen}
  target={selected}
  onClose={() => (actionMenuOpen = false)}
/>

<style>
  .palette {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg, #14161a);
    color: var(--fg, #f5f5f5);
  }
  .body {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .body.single-column :global(.result-list) {
    flex: 1;
  }
  .body.preview-only :global(.preview-pane) {
    flex: 1;
    width: auto;
  }
</style>

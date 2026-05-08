<script lang="ts">
  import { buildBindings, resolveAction } from "../lib/keybindings";
  import { closePalette } from "../lib/commands";
  import { isTauri } from "../lib/tauri";
  import {
    confirmSelection,
    confirmSelectionWithAlternateFormat,
    copySelection,
    deleteSelection,
    togglePinSelection,
  } from "../stores/searchActions";
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
  import OnboardingBanner from "./OnboardingBanner.svelte";
  import PreviewPane from "./PreviewPane.svelte";
  import ResultList from "./ResultList.svelte";
  import SearchBox from "./SearchBox.svelte";
  import StatusBar from "./StatusBar.svelte";

  let actionMenuOpen = $state(false);

  $effect(() => {
    void refreshRecent();
    void refreshSettings();
  });

  const selected = $derived(currentSelection());

  $effect(() => {
    void hydratePreview(selected?.id);
  });

  const handleInput = (next: string): void => {
    scheduleQuery(next);
  };

  const handleConfirm = (index: number): void => {
    selectByIndex(index);
    void confirmSelection();
  };

  const handleSelect = (index: number): void => {
    selectByIndex(index);
  };

  // The settings store re-renders Palette whenever the user flips
  // showPreviewPane or paletteRowCount. Mirroring the value into a $derived
  // keeps the markup readable without re-deriving on each access below.
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
        void confirmSelection();
        break;
      case "confirm-alternate-format":
        void confirmSelectionWithAlternateFormat();
        break;
      case "copy":
        void copySelection();
        break;
      case "open-actions":
        actionMenuOpen = true;
        break;
      case "toggle-pin":
        void togglePinSelection();
        break;
      case "delete":
        void deleteSelection();
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
      case "close":
        if (actionMenuOpen) actionMenuOpen = false;
        else if (previewExpanded) previewExpanded = false;
        else if (isTauri()) void closePalette();
        break;
    }
  };
</script>

<section class="palette" style="--palette-row-count: {paletteRowCount}">
  <SearchBox value={searchState.query} onInput={handleInput} onKeydown={handleKeydown} />
  <OnboardingBanner />
  <div class="body" class:single-column={!showPreviewPane && !previewExpanded} class:preview-only={previewExpanded}>
    {#if !previewExpanded}
      <ResultList
        items={searchState.results}
        selectedIndex={searchState.selectedIndex}
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

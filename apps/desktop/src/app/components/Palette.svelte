<script lang="ts">
  import { resolveAction } from "../lib/keybindings";
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

  const handleKeydown = (event: KeyboardEvent): void => {
    const action = resolveAction(event);
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
      case "open-settings":
        showSettings();
        break;
      case "close":
        if (actionMenuOpen) actionMenuOpen = false;
        else if (isTauri()) void closePalette();
        break;
    }
  };
</script>

<section class="palette">
  <SearchBox value={searchState.query} onInput={handleInput} onKeydown={handleKeydown} />
  <OnboardingBanner />
  <div class="body">
    <ResultList
      items={searchState.results}
      selectedIndex={searchState.selectedIndex}
      onSelect={handleSelect}
      onConfirm={handleConfirm}
    />
    <PreviewPane item={selected} preview={previewState.preview} loading={previewState.loading} errorMessage={previewState.errorMessage} />
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
</style>

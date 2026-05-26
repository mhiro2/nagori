<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import { closePalette, openSettingsWindow } from '../lib/commands';
  import { buildBindings, resolveAction } from '../lib/keybindings';
  import { isTauri, subscribe, TAURI_EVENTS } from '../lib/tauri';
  import {
    capabilitiesState,
    quickLookAvailable,
    refreshCapabilities,
  } from '../stores/capabilities.svelte';
  import {
    confirmSelection,
    confirmSelectionWithAlternateFormat,
    copyMultiSelection,
    copySelection,
    deleteMultiSelection,
    deleteSelection,
    previewSelection,
    togglePinSelection,
  } from '../stores/searchActions';
  import {
    clearMultiSelect,
    multiSelectState,
    rangeSelectMulti,
    selectAllMulti,
    toggleMultiSelect,
  } from '../stores/searchMultiSelect.svelte';
  import { expandPreview, hydratePreview, previewState } from '../stores/searchPreview.svelte';
  import {
    refreshCurrent,
    refreshRecent,
    scheduleQuery,
    searchState,
  } from '../stores/searchQuery.svelte';
  import {
    currentSelection,
    selectByIndex,
    selectFirst,
    selectLast,
    selectNext,
    selectPrev,
  } from '../stores/searchSelection';
  import { refreshSettings, settingsState } from '../stores/settings.svelte';
  import { showSettings } from '../stores/view.svelte';
  import ActionMenu from './ActionMenu.svelte';
  import FilterChips from './FilterChips.svelte';
  import PreviewPane from './PreviewPane.svelte';
  import ResultList from './ResultList.svelte';
  import SearchBox from './SearchBox.svelte';
  import StatusBar from './StatusBar.svelte';

  let actionMenuOpen = $state(false);

  onMount(() => {
    const offClipboardChanged = subscribe<{ entryId: string }>(
      TAURI_EVENTS.clipboardChanged,
      () => {
        void refreshCurrent();
      },
      // Backfill any capture that landed between palette mount and
      // `listen()` resolving; without this the first emit after open
      // can slip through the attach gap.
      () => {
        void refreshCurrent();
      },
    );
    return () => {
      offClipboardChanged();
    };
  });

  $effect(() => {
    void Promise.all([refreshRecent(), refreshSettings(), refreshCapabilities()]);
  });

  const selected = $derived(currentSelection());
  const resultIds = $derived(searchState.results.map((r) => r.id));

  // Debounce so rapid arrow-key navigation across a 50-row list doesn't fire
  // a `get_entry_preview` IPC per row. Only the row the user settles on
  // crosses the bridge.
  //
  // Cleanup lives in `onDestroy`, not in the effect's return: the effect can
  // re-run on unrelated reactive ticks (e.g. `currentSelection()` returning
  // a fresh object with the same id), and tying the cleanup to the effect
  // would clear the in-flight timer before bailing on the unchanged key —
  // leaving no pending hydrate at all. With the effect-scoped cleanup
  // moved out, an unchanged `(id, query)` is a no-op and the previously
  // armed timer keeps running.
  const PREVIEW_DEBOUNCE_MS = 60;
  let previewDebounceTimer: ReturnType<typeof setTimeout> | undefined;
  let lastPreviewKey: string | undefined;
  $effect(() => {
    const id = selected?.id;
    const query = searchState.query;
    const key = `${id ?? ''}\0${query}`;
    if (key === lastPreviewKey) return;
    lastPreviewKey = key;
    if (previewDebounceTimer !== undefined) clearTimeout(previewDebounceTimer);
    previewDebounceTimer = setTimeout(() => {
      previewDebounceTimer = undefined;
      void hydratePreview(id, query);
    }, PREVIEW_DEBOUNCE_MS);
  });
  onDestroy(() => {
    if (previewDebounceTimer !== undefined) {
      clearTimeout(previewDebounceTimer);
      previewDebounceTimer = undefined;
    }
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
  // Pass the platform so user overrides written as `CmdOrCtrl+...` (the canonical
  // wire format from AppSettings) bind to the right physical modifier — Cmd on
  // macOS, Ctrl on Windows/Linux. Falls back to macOS semantics until the
  // capability snapshot hydrates.
  const paletteBindings = $derived(
    buildBindings(
      settingsState.settings?.paletteHotkeys ?? {},
      capabilitiesState.capabilities?.platform,
    ),
  );
  let previewExpanded = $state(false);

  const handleKeydown = (event: KeyboardEvent): void => {
    const action = resolveAction(event, paletteBindings);
    if (!action) return;
    event.preventDefault();
    switch (action) {
      case 'select-next':
        selectNext();
        break;
      case 'select-prev':
        selectPrev();
        break;
      case 'select-first':
        selectFirst();
        break;
      case 'select-last':
        selectLast();
        break;
      case 'confirm':
        if (multiSelectState.selected.size > 0) void copyMultiSelection();
        else void confirmSelection();
        break;
      case 'confirm-alternate-format':
        if (multiSelectState.selected.size > 0) void copyMultiSelection();
        else void confirmSelectionWithAlternateFormat();
        break;
      case 'copy':
        if (multiSelectState.selected.size > 0) void copyMultiSelection();
        else void copySelection();
        break;
      case 'open-actions':
        actionMenuOpen = true;
        break;
      case 'toggle-pin':
        void togglePinSelection();
        break;
      case 'delete':
        if (multiSelectState.selected.size > 0) void deleteMultiSelection();
        else void deleteSelection();
        break;
      case 'clear-query':
        scheduleQuery('');
        break;
      case 'open-preview':
        previewExpanded = !previewExpanded;
        break;
      case 'preview-quick-look':
        if (quickLookAvailable()) void previewSelection();
        break;
      case 'open-settings':
        // Settings is a separate native window under Tauri (own
        // decorations, no always-on-top). Fall back to the in-process
        // viewState toggle in non-Tauri dev/test contexts so the unit
        // tests that spy on `showSettings` still pass.
        if (isTauri()) void openSettingsWindow();
        else showSettings();
        break;
      case 'multi-toggle': {
        const id = currentSelection()?.id;
        if (id !== undefined) toggleMultiSelect(id);
        break;
      }
      case 'multi-select-all':
        selectAllMulti(resultIds);
        break;
      case 'close':
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
  <div
    class="body"
    class:single-column={!showPreviewPane && !previewExpanded}
    class:preview-only={previewExpanded}
  >
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
      <PreviewPane
        item={selected}
        preview={previewState.preview}
        loading={previewState.loading}
        errorMessage={previewState.errorMessage}
        expanded={previewExpanded}
        expandedLoading={previewState.expandedLoading}
        expandedErrorMessage={previewState.expandedErrorMessage}
        onExpandBody={(id) => void expandPreview(id)}
      />
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

<ActionMenu open={actionMenuOpen} target={selected} onClose={() => (actionMenuOpen = false)} />

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

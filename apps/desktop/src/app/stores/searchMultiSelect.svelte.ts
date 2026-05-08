// Multi-select state for the result list. Kept separate from the
// single-cursor selection in `searchSelection.ts` because the cursor
// always points at *one* row (used for preview, single-confirm, etc.)
// while the multi-select set tracks an opt-in "batch" of rows the user
// has marked for bulk copy/delete. Resetting on a new query is the
// caller's responsibility — see `searchQuery.runQuery`.
//
// Storage choice: a plain `Set<string>` re-assigned on every mutation.
// Svelte 5 runes don't observe `Set.add`/`delete` calls, so toggling by
// constructing a fresh set is the simplest way to keep `$derived` views
// in lock-step without pulling in `svelte/reactivity`.

type MultiSelectState = {
  selected: Set<string>;
  /// Anchor for shift-click range selection. The most recently
  /// toggled-on id; range select treats it as the start of the run.
  anchor: string | undefined;
};

export const multiSelectState = $state<MultiSelectState>({
  selected: new Set(),
  anchor: undefined,
});

export const isMultiSelected = (id: string): boolean => multiSelectState.selected.has(id);

export const multiSelectSize = (): number => multiSelectState.selected.size;

export const toggleMultiSelect = (id: string): void => {
  const next = new Set(multiSelectState.selected);
  if (next.has(id)) {
    next.delete(id);
    if (multiSelectState.anchor === id) {
      multiSelectState.anchor = undefined;
    }
  } else {
    next.add(id);
    multiSelectState.anchor = id;
  }
  multiSelectState.selected = next;
};

export const selectAllMulti = (ids: readonly string[]): void => {
  multiSelectState.selected = new Set(ids);
  multiSelectState.anchor = ids.at(-1);
};

export const clearMultiSelect = (): void => {
  if (multiSelectState.selected.size === 0 && multiSelectState.anchor === undefined) return;
  multiSelectState.selected = new Set();
  multiSelectState.anchor = undefined;
};

/// Range-select helper: marks every id between the current anchor and
/// `targetId` (inclusive, in list order) as selected. Falls back to a
/// single-id toggle when no anchor exists yet — without this the first
/// shift-click after a reset would do nothing, which feels broken.
export const rangeSelectMulti = (orderedIds: readonly string[], targetId: string): void => {
  const targetIdx = orderedIds.indexOf(targetId);
  if (targetIdx < 0) return;
  const anchorId = multiSelectState.anchor;
  if (anchorId === undefined) {
    toggleMultiSelect(targetId);
    return;
  }
  const anchorIdx = orderedIds.indexOf(anchorId);
  if (anchorIdx < 0) {
    toggleMultiSelect(targetId);
    return;
  }
  const [start, end] = anchorIdx <= targetIdx ? [anchorIdx, targetIdx] : [targetIdx, anchorIdx];
  const next = new Set(multiSelectState.selected);
  for (let i = start; i <= end; i += 1) {
    const id = orderedIds[i];
    if (id !== undefined) next.add(id);
  }
  multiSelectState.selected = next;
};

/// Drop ids that no longer appear in the result list. Called after a
/// query change wipes the visible rows so the count in the status bar
/// doesn't lie about phantom selections the user can no longer see.
export const reconcileMultiSelect = (visibleIds: readonly string[]): void => {
  if (multiSelectState.selected.size === 0) return;
  const visible = new Set(visibleIds);
  const next = new Set<string>();
  for (const id of multiSelectState.selected) {
    if (visible.has(id)) next.add(id);
  }
  if (next.size === multiSelectState.selected.size) return;
  multiSelectState.selected = next;
  if (multiSelectState.anchor !== undefined && !next.has(multiSelectState.anchor)) {
    multiSelectState.anchor = undefined;
  }
};

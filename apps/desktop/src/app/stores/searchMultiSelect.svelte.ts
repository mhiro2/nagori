// Svelte 5 runes don't observe `Set.add`/`delete`, so every mutation
// re-assigns `selected` to a fresh `Set` to keep `$derived` views in
// lock-step without pulling in `svelte/reactivity`.

type MultiSelectState = {
  selected: Set<string>;
  // Anchor for shift-click range selection — the most recently
  // toggled-on id.
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
  const current = multiSelectState.selected;
  const allCovered = current.size === ids.length && ids.every((id) => current.has(id));
  const lastId = ids.at(-1);
  if (allCovered) {
    if (multiSelectState.anchor !== lastId) multiSelectState.anchor = lastId;
    return;
  }
  multiSelectState.selected = new Set(ids);
  multiSelectState.anchor = lastId;
};

export const clearMultiSelect = (): void => {
  if (multiSelectState.selected.size === 0 && multiSelectState.anchor === undefined) return;
  multiSelectState.selected = new Set();
  multiSelectState.anchor = undefined;
};

// Falls back to a single-id toggle when no anchor exists yet so the
// first shift-click after a reset still does something visible.
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

// Drop ids that no longer appear in the result list so the status-bar
// count doesn't lie about phantom selections the user can't see.
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

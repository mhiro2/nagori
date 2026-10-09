// How the palette behaves when it is shown again. Two reasons to reopen it are
// told apart by how the previous showing ended:
//
// - It ended with an action that hides the palette (a paste or a copy): the
//   user got what they came for, so the next open starts a new paste — the
//   query, filters and multi-selection are cleared and the recent list shows.
// - It was dismissed without acting (Escape, clicking away, the hotkey): the
//   user may come back to the same search, so the next open resumes it with
//   the query selected (typing replaces it, arrows / Enter continue), and any
//   filters kept from last time are called out until the user changes them.
//
// Either way, focus lands in the search box.

export type ReopenMode = 'fresh' | 'resume';

let endedWithAction = false;

export const paletteSessionState = $state<{ filtersRetained: boolean }>({
  filtersRetained: false,
});

/// Run an IPC that hides the palette on success, recording that the session
/// ended with an action. A failure that leaves the palette on screen (it still
/// has focus) is not an ending; one reported after the palette hid — an
/// auto-paste that failed once the copy had landed — still is.
export const runSessionEndingAction = async <T>(action: () => Promise<T>): Promise<T> => {
  endedWithAction = true;
  try {
    return await action();
  } catch (err) {
    if (document.hasFocus()) endedWithAction = false;
    throw err;
  }
};

/// Called when the palette is shown again: which kind of reopen this is.
export const takeReopenMode = (): ReopenMode => {
  const mode: ReopenMode = endedWithAction ? 'fresh' : 'resume';
  endedWithAction = false;
  return mode;
};

export const setFiltersRetained = (retained: boolean): void => {
  paletteSessionState.filtersRetained = retained;
};

/// Test-only reset of the module-private flag.
export const resetPaletteSessionForTest = (): void => {
  endedWithAction = false;
  paletteSessionState.filtersRetained = false;
};

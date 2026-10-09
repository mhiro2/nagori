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
// Session-ending IPCs still awaiting their result.
let inFlight = 0;

export const paletteSessionState = $state<{ filtersRetained: boolean }>({
  filtersRetained: false,
});

/// Run an IPC that hides the palette on success, recording that the session
/// ended with an action once it succeeds. A rejection alone proves nothing
/// either way — a copy can fail before the palette hides, or the user can have
/// clicked away meanwhile — so failures that happen *after* the hide are
/// reported separately by the backend's `paste_failed` event
/// (`noteActionEndedAfterHide`).
export const runSessionEndingAction = async <T>(action: () => Promise<T>): Promise<T> => {
  inFlight += 1;
  try {
    const result = await action();
    endedWithAction = true;
    return result;
  } finally {
    inFlight -= 1;
  }
};

/// The backend reported an auto-paste failure, which it only does after the
/// palette hid and the copy landed. Counts as an ending when it belongs to an
/// action this palette started (a hotkey re-paste with the palette closed does
/// not).
export const noteActionEndedAfterHide = (): void => {
  if (inFlight > 0) endedWithAction = true;
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
  inFlight = 0;
  paletteSessionState.filtersRetained = false;
};

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
// Bumped every time the palette is dismissed. An action that awaits before
// sending its command (settling the search, listing paste options) captures it
// first and gives up if it moved, so an Enter / delete still waiting on a
// search when the user pressed Escape or clicked away never runs once the
// palette is gone. Plain counter (not reactive) — only actions compare it.
let generation = 0;

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

/// The current showing of the palette, to compare against later with
/// `isPaletteSessionCurrent`.
export const paletteSessionToken = (): number => generation;

/// Whether the showing `token` was taken in is still the one on screen.
export const isPaletteSessionCurrent = (token: number): boolean => token === generation;

/// The palette is being dismissed (Escape, clicking away, the close action):
/// actions started while it was up must not reach the backend any more.
export const endPaletteSession = (): void => {
  generation += 1;
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

/// Test-only reset of the module-private state.
export const resetPaletteSessionForTest = (): void => {
  endedWithAction = false;
  inFlight = 0;
  generation = 0;
  paletteSessionState.filtersRetained = false;
};

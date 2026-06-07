// Transient state for the "paste as <format>" picker. The picker is opened
// from the alternate-format paste chord (`Cmd+Shift+Enter`) when the selected
// entry offers more than one pasteable representation; `Palette` renders it
// off this store while `searchActions` drives the open/apply flow.

import type { PasteOption } from '../lib/types';

type PasteFormatPickerState = {
  open: boolean;
  // The entry the picker acts on, captured when it opens. Palette navigation
  // is frozen while the picker is open, so this id can't drift mid-choice.
  targetId: string | undefined;
  // The distinct pasteable representations, in canonical order, as returned by
  // the backend `list_paste_options`.
  options: PasteOption[];
};

export const pasteFormatPickerState = $state<PasteFormatPickerState>({
  open: false,
  targetId: undefined,
  options: [],
});

// Bumped on every dismissal (including the no-op close fired when the palette
// hides). An async opener captures it before awaiting and bails if it changed,
// so a palette hide *during* the options query can't pop the picker open on a
// now-hidden window against a stale target. Plain counter (not reactive) — only
// the opener compares it.
let dismissGeneration = 0;

export const pasteFormatPickerGeneration = (): number => dismissGeneration;

export const openPasteFormatPicker = (targetId: string, options: PasteOption[]): void => {
  pasteFormatPickerState.targetId = targetId;
  pasteFormatPickerState.options = options;
  pasteFormatPickerState.open = true;
};

export const closePasteFormatPicker = (): void => {
  dismissGeneration += 1;
  pasteFormatPickerState.open = false;
  pasteFormatPickerState.targetId = undefined;
  pasteFormatPickerState.options = [];
};

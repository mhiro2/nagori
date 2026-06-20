// Transient state for the per-row right-click context menu. The menu is opened
// from a result row's `contextmenu` event; `Palette` renders it off this store
// while `EntryContextMenu` drives the action dispatch.
//
// The target id(s) are captured when the menu opens and the menu acts on them
// directly — it never reads the live selection. A background refresh
// (clipboard capture / `runQuery`) landing between the right-click and a menu
// click therefore cannot redirect the action onto a different entry.

type EntryContextMenuState = {
  open: boolean;
  // Viewport coordinates of the click, before clamping into the window. The
  // menu component reads these and adjusts so it never overflows the palette.
  x: number;
  y: number;
  // The entries the menu acts on. Length 1 for a plain right-click; the whole
  // multi-selection when the right-clicked row is part of it (filer-style).
  targetIds: string[];
  // Pinned state of the right-clicked row, captured at open, for the
  // pin/unpin label and toggle. Only consulted for a single target.
  primaryPinned: boolean;
  // Whether the right-clicked row can be pasted in more than one format, so the
  // menu shows its *Paste as…* row only when it offers a genuine choice. Only
  // consulted for a single target.
  offersFormatChoice: boolean;
};

export type EntryContextMenuTarget = {
  x: number;
  y: number;
  targetIds: string[];
  primaryPinned: boolean;
  offersFormatChoice: boolean;
};

export const entryContextMenuState = $state<EntryContextMenuState>({
  open: false,
  x: 0,
  y: 0,
  targetIds: [],
  primaryPinned: false,
  offersFormatChoice: false,
});

export const openEntryContextMenu = (target: EntryContextMenuTarget): void => {
  entryContextMenuState.x = target.x;
  entryContextMenuState.y = target.y;
  entryContextMenuState.targetIds = target.targetIds;
  entryContextMenuState.primaryPinned = target.primaryPinned;
  entryContextMenuState.offersFormatChoice = target.offersFormatChoice;
  entryContextMenuState.open = true;
};

export const closeEntryContextMenu = (): void => {
  entryContextMenuState.open = false;
  entryContextMenuState.targetIds = [];
  entryContextMenuState.primaryPinned = false;
  entryContextMenuState.offersFormatChoice = false;
};

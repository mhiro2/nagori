// Keyboard-first bindings for the palette. The actual global shortcut is
// registered on the Rust side via `tauri-plugin-global-shortcut`; the entries
// below describe the in-window navigation contract used by Palette.svelte.
//
// Human-readable labels for each action live in the i18n dictionaries
// (`palette.hints.*` and `keybindings.*`) and are looked up by the consumer.

export type PaletteAction =
  | 'select-next'
  | 'select-prev'
  | 'select-first'
  | 'select-last'
  | 'confirm'
  | 'confirm-alternate-format'
  | 'copy'
  | 'open-actions'
  | 'toggle-pin'
  | 'delete'
  | 'open-settings'
  | 'close';

export type Binding = {
  action: PaletteAction;
  key: string;
  meta?: boolean;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
};

export const PALETTE_BINDINGS: readonly Binding[] = [
  { action: 'select-next', key: 'ArrowDown' },
  { action: 'select-prev', key: 'ArrowUp' },
  { action: 'select-next', key: 'n', ctrl: true },
  { action: 'select-prev', key: 'p', ctrl: true },
  { action: 'select-first', key: 'Home' },
  { action: 'select-last', key: 'End' },
  { action: 'confirm', key: 'Enter' },
  { action: 'confirm-alternate-format', key: 'Enter', meta: true, shift: true },
  { action: 'copy', key: 'Enter', meta: true },
  { action: 'open-actions', key: 'k', meta: true },
  { action: 'toggle-pin', key: 'p', meta: true },
  { action: 'delete', key: 'Backspace', meta: true },
  { action: 'open-settings', key: ',', meta: true },
  { action: 'close', key: 'Escape' },
];

const matches = (binding: Binding, event: KeyboardEvent): boolean =>
  binding.key === event.key &&
  (binding.meta ?? false) === (event.metaKey || false) &&
  (binding.ctrl ?? false) === (event.ctrlKey || false) &&
  (binding.shift ?? false) === (event.shiftKey || false) &&
  (binding.alt ?? false) === (event.altKey || false);

export const resolveAction = (event: KeyboardEvent): PaletteAction | undefined =>
  PALETTE_BINDINGS.find((b) => matches(b, event))?.action;

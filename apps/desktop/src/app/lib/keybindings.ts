// Keyboard-first bindings for the palette. The actual global shortcut is
// registered on the Rust side via `tauri-plugin-global-shortcut`; the entries
// below describe the in-window navigation contract used by Palette.svelte.
//
// Human-readable labels for each action live in the i18n dictionaries
// (`palette.hints.*` and `keybindings.*`) and are looked up by the consumer.

import type { PaletteHotkeyAction } from './types';

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
  | 'clear-query'
  | 'open-preview'
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

// Overrides target the same `PaletteAction` codes via the
// `PaletteHotkeyAction` enum (kebab-case wire format). The mapping is
// intentionally sparse so user settings only carry actual changes; the
// in-palette behaviour for the four overridable actions is otherwise
// served by the entries in `PALETTE_BINDINGS`.
const ACTION_FROM_OVERRIDE: Record<PaletteHotkeyAction, PaletteAction> = {
  pin: 'toggle-pin',
  delete: 'delete',
  'paste-as-plain': 'confirm-alternate-format',
  'copy-without-paste': 'copy',
  clear: 'clear-query',
  'open-preview': 'open-preview',
};

const isPaletteHotkeyAction = (value: string): value is PaletteHotkeyAction =>
  Object.prototype.hasOwnProperty.call(ACTION_FROM_OVERRIDE, value);

// Always compare keys case-insensitively so a binding stored as `p` still
// matches `event.key === 'P'` when Shift is held — `KeyboardEvent.key`
// follows the OS' shifted output, which would otherwise dodge the binding.
const matches = (binding: Binding, event: KeyboardEvent): boolean =>
  binding.key.toLowerCase() === event.key.toLowerCase() &&
  (binding.meta ?? false) === (event.metaKey || false) &&
  (binding.ctrl ?? false) === (event.ctrlKey || false) &&
  (binding.shift ?? false) === (event.shiftKey || false) &&
  (binding.alt ?? false) === (event.altKey || false);

const sameShortcut = (a: Binding, b: Binding): boolean =>
  a.key.toLowerCase() === b.key.toLowerCase() &&
  (a.meta ?? false) === (b.meta ?? false) &&
  (a.ctrl ?? false) === (b.ctrl ?? false) &&
  (a.shift ?? false) === (b.shift ?? false) &&
  (a.alt ?? false) === (b.alt ?? false);

// Multi-character keys we accept verbatim. Restricted to the `KeyboardEvent.key`
// names the palette actually reacts to so an unknown token (e.g. `Command`,
// which is not a recognised modifier alias) gets rejected at parse time
// instead of silently being stored as the `key` field. Without this guard,
// `Command+Backspace` would parse as a bare `Backspace` binding because
// `Command` isn't a known modifier and falls through to the `key` slot.
const NAMED_KEYS: ReadonlySet<string> = new Set([
  'ArrowUp',
  'ArrowDown',
  'ArrowLeft',
  'ArrowRight',
  'Enter',
  'Escape',
  'Tab',
  'Space',
  'Backspace',
  'Delete',
  'Insert',
  'Home',
  'End',
  'PageUp',
  'PageDown',
  'F1',
  'F2',
  'F3',
  'F4',
  'F5',
  'F6',
  'F7',
  'F8',
  'F9',
  'F10',
  'F11',
  'F12',
  'F13',
  'F14',
  'F15',
  'F16',
  'F17',
  'F18',
  'F19',
  'F20',
]);

/// Parse an accelerator string like `Cmd+Shift+P` into a `Binding`. Returns
/// `null` for accelerators with no key segment, with unsupported tokens, or
/// with multiple key segments — the caller falls back to defaults in that
/// case rather than rendering a silently broken hotkey.
export const parseAccelerator = (action: PaletteAction, accelerator: string): Binding | null => {
  const tokens = accelerator
    .split('+')
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  if (tokens.length === 0) return null;
  const binding: Binding = { action, key: '' };
  for (const token of tokens) {
    const lower = token.toLowerCase();
    if (lower === 'cmd' || lower === 'meta' || lower === 'cmdorctrl') {
      if (binding.meta) return null;
      binding.meta = true;
    } else if (lower === 'ctrl' || lower === 'control') {
      if (binding.ctrl) return null;
      binding.ctrl = true;
    } else if (lower === 'shift') {
      if (binding.shift) return null;
      binding.shift = true;
    } else if (lower === 'alt' || lower === 'option' || lower === 'opt') {
      if (binding.alt) return null;
      binding.alt = true;
    } else if (binding.key) {
      // Already saw a key segment — multiple keys are not a thing.
      return null;
    } else if (token.length === 1) {
      binding.key = token.toLowerCase();
    } else if (NAMED_KEYS.has(token)) {
      binding.key = token;
    } else {
      // Unknown multi-char token (e.g. `Command`, a misspelling, or a
      // platform-specific name we don't yet support).
      return null;
    }
  }
  return binding.key ? binding : null;
};

/// Build the effective binding list by overlaying user overrides on top of
/// the defaults. Defaults are dropped for any `PaletteAction` whose
/// override resolves cleanly so the new accelerator wins outright; if the
/// user's accelerator string doesn't parse, the default for that action
/// stays in place. We additionally drop default bindings whose key+modifier
/// shape collides with any successful override — otherwise the matcher
/// would fire the (still-present) default first and shadow the user's
/// remap. Overrides that collide with each other are *all* discarded so
/// neither action silently shadows the other; the affected actions fall
/// back to their defaults (after the default-vs-replacement filter) and
/// the misconfiguration surfaces as "neither remap is active" rather than
/// as a single mysterious gap.
export const buildBindings = (
  overrides: Partial<Record<PaletteHotkeyAction, string>>,
): readonly Binding[] => {
  const parsed = new Map<PaletteAction, Binding>();
  for (const [override, accel] of Object.entries(overrides)) {
    if (!accel || !isPaletteHotkeyAction(override)) continue;
    const action = ACTION_FROM_OVERRIDE[override];
    const binding = parseAccelerator(action, accel);
    if (binding) parsed.set(action, binding);
  }
  const parsedBindings = Array.from(parsed.values());
  const colliding = new Set<PaletteAction>();
  for (let i = 0; i < parsedBindings.length; i += 1) {
    const a = parsedBindings[i];
    if (!a) continue;
    for (let j = i + 1; j < parsedBindings.length; j += 1) {
      const b = parsedBindings[j];
      if (!b) continue;
      if (sameShortcut(a, b)) {
        colliding.add(a.action);
        colliding.add(b.action);
      }
    }
  }
  const replacements = parsedBindings.filter((b) => !colliding.has(b.action));
  const replacedActions = new Set(replacements.map((b) => b.action));
  const overlaid: Binding[] = PALETTE_BINDINGS.filter(
    (b) => !replacedActions.has(b.action) && !replacements.some((r) => sameShortcut(b, r)),
  );
  overlaid.push(...replacements);
  return overlaid;
};

export const resolveAction = (
  event: KeyboardEvent,
  bindings: readonly Binding[] = PALETTE_BINDINGS,
): PaletteAction | undefined => bindings.find((b) => matches(b, event))?.action;

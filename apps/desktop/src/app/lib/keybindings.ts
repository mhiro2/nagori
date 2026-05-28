// Keyboard-first bindings for the palette. The actual global shortcut is
// registered on the Rust side via `tauri-plugin-global-shortcut`; the entries
// below describe the in-window navigation contract used by Palette.svelte.
//
// Human-readable labels for each action live in the i18n dictionaries
// (`palette.hints.*` and `keybindings.*`) and are looked up by the consumer.

import { isTauri } from './tauri';
import type { PaletteHotkeyAction, Platform } from './types';

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
  | 'preview-quick-look'
  | 'open-settings'
  | 'multi-toggle'
  | 'multi-select-all'
  | 'close';

export type Binding = {
  action: PaletteAction;
  key: string;
  meta?: boolean;
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
};

// Defaults are written with `meta: true` standing in for the *primary OS
// modifier* — Cmd on macOS, Ctrl on Windows/Linux. `paletteBindingsFor`
// rewrites those to `ctrl: true` on non-mac hosts so the same logical
// binding fires under the modifier that platform's users actually press.
// The literal `ctrl: true` entries (Ctrl+N / Ctrl+P) stay verbatim on
// every platform — those come from the Emacs convention and are
// independent of the primary-modifier swap.
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
  // `j` is unused as a shortcut elsewhere in the palette and Cmd+J has
  // no OS-reserved meaning on macOS, so it can serve as the keyboard
  // toggle for multi-select without stealing typing in the search box.
  // (Plain Space would conflict with typing a literal space; Cmd+A
  // would break the input's native select-all-text behaviour.)
  { action: 'multi-toggle', key: 'j', meta: true },
  { action: 'multi-select-all', key: 'a', meta: true, shift: true },
  // Cmd+Y matches the Finder "Quick Look" menu accelerator; plain Space
  // would conflict with typing a literal space into the search box and is
  // intentionally avoided here for the same reason multi-toggle uses
  // Cmd+J rather than Space.
  { action: 'preview-quick-look', key: 'y', meta: true },
  { action: 'close', key: 'Escape' },
];

// Resolve the default binding set for a given platform. The static
// `PALETTE_BINDINGS` list above expresses every primary-modifier binding
// as `meta: true` (mac-shaped); this swaps to `ctrl: true` on
// Windows/Linux so the literal modifier the user presses actually
// matches. The Emacs-style `Ctrl+N` / `Ctrl+P` navigation bindings are
// dropped on non-mac hosts because the Cmd→Ctrl swap collides them with
// `toggle-pin` (Cmd+P) and `select-next`/`select-prev` — Arrow keys
// continue to cover that affordance on every platform.
export const paletteBindingsFor = (platform: Platform | undefined): readonly Binding[] => {
  if (macOsLikePlatform(platform)) return PALETTE_BINDINGS;
  const out: Binding[] = [];
  for (const b of PALETTE_BINDINGS) {
    if (b.ctrl === true && b.meta !== true && (b.key === 'n' || b.key === 'p')) continue;
    if (b.meta) {
      const swapped: Binding = { action: b.action, key: b.key, ctrl: true };
      if (b.shift !== undefined) swapped.shift = b.shift;
      if (b.alt !== undefined) swapped.alt = b.alt;
      out.push(swapped);
    } else {
      out.push(b);
    }
  }
  return out;
};

// Modifier the platform treats as its "primary" accelerator — Cmd on
// macOS, Ctrl on Windows/Linux. Shared by mouse handlers (ctrl/⌘-click
// multi-select) and any non-keyboard surface that needs the same
// per-platform behaviour the default bindings give the keyboard.
export const isPrimaryModifierHeld = (
  event: { metaKey?: boolean; ctrlKey?: boolean },
  platform: Platform | undefined,
): boolean => (macOsLikePlatform(platform) ? !!event.metaKey : !!event.ctrlKey);

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

// Pre-hydration platform hint derived from `navigator.userAgent` so the
// palette bindings, hint glyphs, and hotkey-edit UI use the right modifier
// from the very first keystroke — `getCapabilities` round-trips through
// the daemon and isn't ready until a few frames after mount, which would
// otherwise show Win/Linux users the macOS `Meta` form briefly. Gated on
// `isTauri()` so jsdom / Storybook contexts keep the documented macOS
// fallback (their UA strings are not the host platform's).
const detectPlatformFromNavigator = (): Platform | undefined => {
  if (!isTauri()) return undefined;
  if (typeof navigator === 'undefined') return undefined;
  const ua = navigator.userAgent;
  if (!ua) return undefined;
  if (/Mac|iPad|iPhone|iPod/i.test(ua)) return 'macos';
  if (/Windows/i.test(ua)) return 'windows';
  if (/Linux|X11|CrOS/i.test(ua)) return 'linuxWayland';
  return undefined;
};

// `CmdOrCtrl` is the canonical wire format used by tauri-plugin-global-shortcut
// and the AppSettings layer (`crates/nagori-core/src/settings.rs`). The shortcut
// plugin resolves it to Cmd on macOS and Ctrl elsewhere; the frontend must do
// the same so user overrides actually fire on Windows/Linux. `Cmd` / `Meta`
// stay bound to the Meta key on all platforms because users who type those
// names are asking for that specific physical key.
const macOsLikePlatform = (platform: Platform | undefined): boolean => {
  // Authoritative snapshot already loaded — trust it verbatim.
  if (platform !== undefined) return platform === 'macos';
  // Pre-hydration: use the UA-derived hint so Win/Linux callers don't get
  // a macOS-shaped binding for the first few frames. Falls through to
  // `true` (the historical default) only when even the UA hint is absent
  // (SSR / unit tests), where the binding shape is incidental anyway.
  const hint = detectPlatformFromNavigator();
  if (hint !== undefined) return hint === 'macos';
  return true;
};

/// Parse an accelerator string like `Cmd+Shift+P` into a `Binding`. Returns
/// `null` for accelerators with no key segment, with unsupported tokens, or
/// with multiple key segments — the caller falls back to defaults in that
/// case rather than rendering a silently broken hotkey.
export const parseAccelerator = (
  action: PaletteAction,
  accelerator: string,
  platform?: Platform,
): Binding | null => {
  const tokens = accelerator
    .split('+')
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  if (tokens.length === 0) return null;
  const binding: Binding = { action, key: '' };
  const cmdOrCtrlIsMeta = macOsLikePlatform(platform);
  for (const token of tokens) {
    const lower = token.toLowerCase();
    if (lower === 'cmd' || lower === 'meta') {
      if (binding.meta) return null;
      binding.meta = true;
    } else if (lower === 'cmdorctrl') {
      if (cmdOrCtrlIsMeta) {
        if (binding.meta) return null;
        binding.meta = true;
      } else {
        if (binding.ctrl) return null;
        binding.ctrl = true;
      }
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
  platform?: Platform,
): readonly Binding[] => {
  const parsed = new Map<PaletteAction, Binding>();
  for (const [override, accel] of Object.entries(overrides)) {
    if (!accel || !isPaletteHotkeyAction(override)) continue;
    const action = ACTION_FROM_OVERRIDE[override];
    const binding = parseAccelerator(action, accel, platform);
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
  const overlaid: Binding[] = paletteBindingsFor(platform).filter(
    (b) => !replacedActions.has(b.action) && !replacements.some((r) => sameShortcut(b, r)),
  );
  overlaid.push(...replacements);
  return overlaid;
};

export const resolveAction = (
  event: KeyboardEvent,
  bindings: readonly Binding[] = PALETTE_BINDINGS,
): PaletteAction | undefined => bindings.find((b) => matches(b, event))?.action;

// Display glyphs follow the Apple HIG convention (Ctrl → Opt → Shift → Cmd →
// key, no separator). On non-Apple platforms we stay with the plain
// `Ctrl+Shift+V` rendering that menubars on those OSes already use, so the
// pill shown in Settings matches what the rest of the system shows.
const MAC_MODIFIER_GLYPHS = {
  ctrl: '⌃', // ⌃
  alt: '⌥', // ⌥
  shift: '⇧', // ⇧
  meta: '⌘', // ⌘
} as const;

// Display labels for multi-char keys, shared by mac glyph mode and the
// `Ctrl+Shift+V` join mode. We map both the Tauri-style tokens (`Up`,
// `PageUp`) and the DOM `KeyboardEvent.key` names (`ArrowUp`) the palette
// override format uses, since the formatter renders both kinds of stored
// strings.
const DISPLAY_KEY_LABELS: Record<string, { mac: string; other: string }> = {
  Space: { mac: 'Space', other: 'Space' },
  Enter: { mac: '↩', other: 'Enter' }, // ↩
  Return: { mac: '↩', other: 'Return' },
  Tab: { mac: '⇥', other: 'Tab' }, // ⇥
  Esc: { mac: '⎋', other: 'Esc' }, // ⎋
  Escape: { mac: '⎋', other: 'Esc' },
  Backspace: { mac: '⌫', other: 'Backspace' }, // ⌫
  Delete: { mac: '⌦', other: 'Delete' }, // ⌦
  Insert: { mac: 'Insert', other: 'Insert' },
  Up: { mac: '↑', other: 'Up' }, // ↑
  Down: { mac: '↓', other: 'Down' }, // ↓
  Left: { mac: '←', other: 'Left' }, // ←
  Right: { mac: '→', other: 'Right' }, // →
  ArrowUp: { mac: '↑', other: 'Up' },
  ArrowDown: { mac: '↓', other: 'Down' },
  ArrowLeft: { mac: '←', other: 'Left' },
  ArrowRight: { mac: '→', other: 'Right' },
  Home: { mac: 'Home', other: 'Home' },
  End: { mac: 'End', other: 'End' },
  PageUp: { mac: 'PgUp', other: 'PgUp' },
  PageDown: { mac: 'PgDn', other: 'PgDn' },
};

const formatKeyForDisplay = (key: string, isMac: boolean): string => {
  if (key.length === 1) return key.toUpperCase();
  const entry = DISPLAY_KEY_LABELS[key];
  if (entry) return isMac ? entry.mac : entry.other;
  return key;
};

/// Render an accelerator (Tauri wire format or palette-binding format) in the
/// platform's idiomatic style. macOS uses contiguous glyphs (`⌘⇧V`); other
/// platforms use `Ctrl+Shift+V`. `CmdOrCtrl` expands to the platform primary
/// modifier (Cmd on macOS, Ctrl elsewhere), matching how the shortcut
/// actually fires at runtime. Returns the input unchanged when parsing
/// fails so a malformed stored value remains visible to the user instead of
/// silently collapsing to empty.
export const formatAccelerator = (accelerator: string, platform?: Platform): string => {
  const tokens = accelerator
    .split('+')
    .map((part) => part.trim())
    .filter((part) => part.length > 0);
  if (tokens.length === 0) return '';
  const isMac = macOsLikePlatform(platform);
  const mods = { meta: false, ctrl: false, alt: false, shift: false };
  let key: string | null = null;
  for (const token of tokens) {
    const lower = token.toLowerCase();
    if (lower === 'cmd' || lower === 'meta' || lower === 'command' || lower === 'super') {
      mods.meta = true;
    } else if (lower === 'win' || lower === 'windows') {
      mods.meta = true;
    } else if (lower === 'cmdorctrl' || lower === 'commandorcontrol') {
      if (isMac) mods.meta = true;
      else mods.ctrl = true;
    } else if (lower === 'ctrl' || lower === 'control') {
      mods.ctrl = true;
    } else if (lower === 'shift') {
      mods.shift = true;
    } else if (lower === 'alt' || lower === 'option' || lower === 'opt') {
      mods.alt = true;
    } else if (key !== null) {
      // Multiple key segments — malformed; surface verbatim rather than
      // silently dropping the second key.
      return accelerator;
    } else {
      key = token;
    }
  }
  if (key === null) return accelerator;
  const displayKey = formatKeyForDisplay(key, isMac);
  if (isMac) {
    const parts: string[] = [];
    if (mods.ctrl) parts.push(MAC_MODIFIER_GLYPHS.ctrl);
    if (mods.alt) parts.push(MAC_MODIFIER_GLYPHS.alt);
    if (mods.shift) parts.push(MAC_MODIFIER_GLYPHS.shift);
    if (mods.meta) parts.push(MAC_MODIFIER_GLYPHS.meta);
    parts.push(displayKey);
    return parts.join('');
  }
  const parts: string[] = [];
  if (mods.ctrl) parts.push('Ctrl');
  // On Windows/Linux, the Meta/Super key is rarely a usable shortcut modifier
  // (the OS intercepts most Win-key combos) — render it explicitly so the
  // user sees that the stored binding may not actually fire.
  if (mods.meta) parts.push(platform === 'windows' ? 'Win' : 'Super');
  if (mods.alt) parts.push('Alt');
  if (mods.shift) parts.push('Shift');
  parts.push(displayKey);
  return parts.join('+');
};

// Capture target controls the wire format we emit. Tauri's global-shortcut
// parser accepts short names like `Up` / `Esc` / `PageUp`; the in-palette
// matcher compares against `KeyboardEvent.key` and so needs the DOM-style
// names like `ArrowUp` / `Escape`. The two grammars overlap for letters,
// digits, punctuation, function keys, Space/Enter/Tab/Backspace/Delete —
// the divergence only matters for arrows / Escape / paging keys, but we
// still have to pick one per call site.
export type CaptureTarget = 'tauri-global' | 'palette-binding';

const TAURI_KEY_FROM_CODE: Record<string, string> = {
  Space: 'Space',
  Enter: 'Enter',
  NumpadEnter: 'Enter',
  Tab: 'Tab',
  Escape: 'Esc',
  Backspace: 'Backspace',
  Delete: 'Delete',
  Insert: 'Insert',
  ArrowUp: 'Up',
  ArrowDown: 'Down',
  ArrowLeft: 'Left',
  ArrowRight: 'Right',
  Home: 'Home',
  End: 'End',
  PageUp: 'PageUp',
  PageDown: 'PageDown',
  Comma: ',',
  Period: '.',
  Slash: '/',
  Semicolon: ';',
  Quote: "'",
  BracketLeft: '[',
  BracketRight: ']',
  Backslash: '\\',
  Minus: '-',
  Equal: '=',
  Backquote: '`',
};

const PALETTE_KEY_FROM_CODE: Record<string, string> = {
  ...TAURI_KEY_FROM_CODE,
  // Palette overrides are compared against `KeyboardEvent.key`, which uses
  // the long names — swap the four divergent codes back to DOM form.
  Escape: 'Escape',
  ArrowUp: 'ArrowUp',
  ArrowDown: 'ArrowDown',
  ArrowLeft: 'ArrowLeft',
  ArrowRight: 'ArrowRight',
};

const MODIFIER_EVENT_KEYS: ReadonlySet<string> = new Set([
  'Meta',
  'Control',
  'Alt',
  'AltGraph',
  'Shift',
  'OS',
  'Hyper',
  'Super',
]);

const keyFromCode = (event: KeyboardEvent, target: CaptureTarget): string | null => {
  const code = event.code;
  if (!code) return null;
  // Palette overrides are matched against `KeyboardEvent.key`, so the stored
  // key must be whatever `event.key` will surface at match time. For
  // printable single-character keys we therefore record `event.key` itself
  // (e.g. `Shift+1` → `!` on US, `Shift+/` → `?`, AZERTY `KeyA` → `q`).
  // Named keys (arrows, Escape, function keys) fall through to the
  // code-based map below where the matcher and the recorder agree on the
  // long DOM name.
  if (target === 'palette-binding') {
    const k = event.key;
    if (k && k.length === 1 && k !== ' ') return k;
    return PALETTE_KEY_FROM_CODE[code] ?? null;
  }
  // tauri-global: bind by physical key so the OS layer registers the same
  // accelerator regardless of which character the shifted/altGr state would
  // produce. Tauri's parser is happy with `Shift+1` and resolves it against
  // the physical Digit1 row on every layout.
  // Letter keys: `KeyA`..`KeyZ` → `A`..`Z`. Storing uppercase matches both
  // Tauri's accelerator format (case-insensitive but conventionally upper)
  // and the palette matcher (`matches` compares case-insensitively).
  if (code.startsWith('Key') && code.length === 4) return code.slice(3);
  // Digits: `Digit1` → `1`. NumpadX is intentionally not folded into the
  // top-row digit — the OS treats them as distinct physical keys for
  // shortcut binding purposes.
  if (code.startsWith('Digit') && code.length === 6) return code.slice(5);
  if (/^F([1-9]|1\d|2[0-4])$/.test(code)) return code;
  return TAURI_KEY_FROM_CODE[code] ?? null;
};

/// Build a wire-format accelerator string from a live `keydown` event. The
/// returned value follows Tauri's `CmdOrCtrl+Shift+V` grammar so it can be
/// persisted directly. Returns `null` for events that carry no usable key
/// segment — pure modifier presses (Cmd alone, Shift alone) and codes we
/// don't recognise both fall in that bucket so the caller knows to keep
/// waiting for the next event instead of committing a half-typed combo.
///
/// The primary OS modifier (Cmd on macOS, Ctrl elsewhere) is folded into
/// `CmdOrCtrl` so a binding recorded on one host stays portable when the
/// settings file syncs to another. The non-primary modifier is preserved
/// verbatim — explicit `Ctrl` on macOS, explicit `Win` on Windows — so a
/// user who deliberately recorded a host-specific combo keeps it.
export const captureFromKeyboardEvent = (
  event: KeyboardEvent,
  target: CaptureTarget,
  platform?: Platform,
): string | null => {
  if (MODIFIER_EVENT_KEYS.has(event.key)) return null;
  const keyToken = keyFromCode(event, target);
  if (keyToken === null) return null;
  const isMac = macOsLikePlatform(platform);
  const tokens: string[] = [];
  const primaryHeld = isMac ? event.metaKey : event.ctrlKey;
  const nonPrimaryHeld = isMac ? event.ctrlKey : event.metaKey;
  if (primaryHeld) tokens.push('CmdOrCtrl');
  if (nonPrimaryHeld) {
    // On macOS the literal Ctrl key stays as `Ctrl`; on Windows/Linux the
    // literal Meta/Win key is rendered as `Win` because Tauri's parser
    // accepts `Super` and `Win` interchangeably and the latter matches
    // what users see on their keyboard.
    tokens.push(isMac ? 'Ctrl' : 'Win');
  }
  if (event.altKey) tokens.push('Alt');
  if (event.shiftKey) tokens.push('Shift');
  tokens.push(keyToken);
  return tokens.join('+');
};

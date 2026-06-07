import { describe, expect, it } from 'vitest';

import {
  buildBindings,
  captureFromKeyboardEvent,
  defaultPaletteAccelerator,
  formatAccelerator,
  isImeComposing,
  isPrimaryModifierHeld,
  resolveAction,
} from './keybindings';

const event = (init: KeyboardEventInit & { key: string }): KeyboardEvent =>
  new KeyboardEvent('keydown', init);

const captureEvent = (init: KeyboardEventInit & { key: string; code?: string }): KeyboardEvent =>
  new KeyboardEvent('keydown', init);

describe('resolveAction', () => {
  it('maps ArrowDown / ArrowUp to next / prev', () => {
    expect(resolveAction(event({ key: 'ArrowDown' }))).toBe('select-next');
    expect(resolveAction(event({ key: 'ArrowUp' }))).toBe('select-prev');
  });

  it('maps Ctrl+N / Ctrl+P to next / prev', () => {
    expect(resolveAction(event({ key: 'n', ctrlKey: true }))).toBe('select-next');
    expect(resolveAction(event({ key: 'p', ctrlKey: true }))).toBe('select-prev');
  });

  it('requires Ctrl for the n/p shortcuts', () => {
    expect(resolveAction(event({ key: 'n' }))).toBeUndefined();
    expect(resolveAction(event({ key: 'p' }))).toBeUndefined();
  });

  it('maps Home / End to first / last', () => {
    expect(resolveAction(event({ key: 'Home' }))).toBe('select-first');
    expect(resolveAction(event({ key: 'End' }))).toBe('select-last');
  });

  it('maps Enter to confirm', () => {
    expect(resolveAction(event({ key: 'Enter' }))).toBe('confirm');
  });

  it('maps Cmd+Enter to copy without paste', () => {
    expect(resolveAction(event({ key: 'Enter', metaKey: true }))).toBe('copy');
  });

  it('maps Cmd+K to open-actions and rejects bare K', () => {
    expect(resolveAction(event({ key: 'k', metaKey: true }))).toBe('open-actions');
    expect(resolveAction(event({ key: 'k' }))).toBeUndefined();
  });

  it('maps Cmd+P to toggle-pin (distinct from Ctrl+P)', () => {
    expect(resolveAction(event({ key: 'p', metaKey: true }))).toBe('toggle-pin');
    expect(resolveAction(event({ key: 'p', ctrlKey: true }))).toBe('select-prev');
  });

  it('maps Cmd+Backspace to delete', () => {
    expect(resolveAction(event({ key: 'Backspace', metaKey: true }))).toBe('delete');
    expect(resolveAction(event({ key: 'Backspace' }))).toBeUndefined();
  });

  it('maps Cmd+, to open-settings', () => {
    expect(resolveAction(event({ key: ',', metaKey: true }))).toBe('open-settings');
  });

  it('maps Cmd+Y to preview-quick-look (rejects bare Y)', () => {
    expect(resolveAction(event({ key: 'y', metaKey: true }))).toBe('preview-quick-look');
    expect(resolveAction(event({ key: 'y' }))).toBeUndefined();
  });

  it('maps Escape to close, regardless of modifiers', () => {
    expect(resolveAction(event({ key: 'Escape' }))).toBe('close');
  });

  it('rejects unknown keys', () => {
    expect(resolveAction(event({ key: 'F13' }))).toBeUndefined();
    expect(resolveAction(event({ key: 'z', metaKey: true, shiftKey: true }))).toBeUndefined();
  });

  it('ignores bindings when modifier set differs from spec', () => {
    // Cmd+K is bound, Cmd+Shift+K is not.
    expect(resolveAction(event({ key: 'k', metaKey: true, shiftKey: true }))).toBeUndefined();
  });

  it('matches shifted single-char keys case-insensitively', () => {
    // KeyboardEvent.key reports the shifted form, so a binding stored as
    // lowercase `p` must still match `event.key === 'P'`.
    const overlaid = buildBindings({ pin: 'Cmd+Shift+P' });
    expect(resolveAction(event({ key: 'P', metaKey: true, shiftKey: true }), overlaid)).toBe(
      'toggle-pin',
    );
  });
});

describe('isImeComposing', () => {
  it('flags keystrokes that are part of an IME composition', () => {
    expect(isImeComposing(event({ key: 'Enter', isComposing: true }))).toBe(true);
  });

  it('treats the legacy keyCode 229 marker as composing', () => {
    // Some engines clear `isComposing` on the committing keystroke but still
    // report the keyCode 229 placeholder.
    expect(isImeComposing(event({ key: 'Enter', keyCode: 229 }))).toBe(true);
  });

  it('passes through ordinary keystrokes', () => {
    expect(isImeComposing(event({ key: 'Enter' }))).toBe(false);
    expect(isImeComposing(event({ key: 'Escape' }))).toBe(false);
  });
});

describe('buildBindings', () => {
  it('drops default bindings whose accelerator collides with an override', () => {
    // Remapping `delete` to Cmd+P should evict the default Cmd+P (toggle-pin)
    // so the override actually wins; otherwise the default fires first and
    // the user's remap is silently shadowed.
    const overlaid = buildBindings({ delete: 'Cmd+P' });
    expect(resolveAction(event({ key: 'p', metaKey: true }), overlaid)).toBe('delete');
  });

  it('keeps the default binding when an override fails to parse', () => {
    // An unparseable accelerator (e.g. only modifiers, no key) must not
    // wipe out the default — otherwise a typo locks the user out of the
    // action entirely.
    const overlaid = buildBindings({ pin: 'Cmd+' });
    expect(resolveAction(event({ key: 'p', metaKey: true }), overlaid)).toBe('toggle-pin');
  });

  it('rejects accelerators with unknown modifier-shaped tokens', () => {
    // `Command` is a common alias users reach for, but our parser only
    // recognises `Cmd` / `Meta` / `CmdOrCtrl`. Without strict rejection,
    // `Command+Backspace` would fall through and store `Backspace` as the
    // key with no modifiers — pressing Backspace alone in the search box
    // would then trigger the override.
    const overlaid = buildBindings({ delete: 'Command+Backspace' });
    expect(resolveAction(event({ key: 'Backspace' }), overlaid)).toBeUndefined();
    // Default Cmd+Backspace remains intact.
    expect(resolveAction(event({ key: 'Backspace', metaKey: true }), overlaid)).toBe('delete');
  });

  it('drops both overrides when they collide on the same shortcut', () => {
    // Two custom hotkeys aimed at the same combo would otherwise leave one
    // action silently unreachable; dropping both restores defaults and
    // makes the misconfiguration visible to the user.
    const overlaid = buildBindings({ pin: 'Cmd+P', delete: 'Cmd+P' });
    // Default Cmd+P (toggle-pin) and default Cmd+Backspace (delete) both
    // come back because neither override survived the collision check.
    expect(resolveAction(event({ key: 'p', metaKey: true }), overlaid)).toBe('toggle-pin');
    expect(resolveAction(event({ key: 'Backspace', metaKey: true }), overlaid)).toBe('delete');
  });

  it('expands CmdOrCtrl to Meta on macOS', () => {
    const overlaid = buildBindings({ pin: 'CmdOrCtrl+I' }, 'macos');
    expect(resolveAction(event({ key: 'i', metaKey: true }), overlaid)).toBe('toggle-pin');
    expect(resolveAction(event({ key: 'i', ctrlKey: true }), overlaid)).toBeUndefined();
  });

  it('expands CmdOrCtrl to Ctrl on Windows', () => {
    const overlaid = buildBindings({ pin: 'CmdOrCtrl+I' }, 'windows');
    expect(resolveAction(event({ key: 'i', ctrlKey: true }), overlaid)).toBe('toggle-pin');
    expect(resolveAction(event({ key: 'i', metaKey: true }), overlaid)).toBeUndefined();
  });

  it('expands CmdOrCtrl to Ctrl on Linux Wayland', () => {
    const overlaid = buildBindings({ pin: 'CmdOrCtrl+I' }, 'linuxWayland');
    expect(resolveAction(event({ key: 'i', ctrlKey: true }), overlaid)).toBe('toggle-pin');
  });

  it('keeps explicit Cmd as Meta even on non-macOS platforms', () => {
    // Users who type `Cmd` are asking for the physical Meta/Win key; only
    // the portable `CmdOrCtrl` alias should swap modifiers per platform.
    const overlaid = buildBindings({ pin: 'Cmd+I' }, 'windows');
    expect(resolveAction(event({ key: 'i', metaKey: true }), overlaid)).toBe('toggle-pin');
    expect(resolveAction(event({ key: 'i', ctrlKey: true }), overlaid)).toBeUndefined();
  });

  it('defaults to macOS semantics when platform is undefined', () => {
    // Capability snapshot is hydrated asynchronously; before it lands we keep
    // the historical default (Meta) so the macOS-focused default bindings
    // still parse correctly.
    const overlaid = buildBindings({ pin: 'CmdOrCtrl+I' });
    expect(resolveAction(event({ key: 'i', metaKey: true }), overlaid)).toBe('toggle-pin');
  });

  it('swaps default primary-modifier bindings to Ctrl on Windows', () => {
    // PALETTE_BINDINGS is mac-shaped (`meta: true`); on non-mac the same
    // logical bindings must fire under Ctrl instead, otherwise Win/Linux
    // users would have no working palette accelerators at all (Win+K is OS-
    // reserved on Windows and doesn't reach the webview).
    const overlaid = buildBindings({}, 'windows');
    expect(resolveAction(event({ key: 'k', ctrlKey: true }), overlaid)).toBe('open-actions');
    expect(resolveAction(event({ key: ',', ctrlKey: true }), overlaid)).toBe('open-settings');
    expect(resolveAction(event({ key: 'p', ctrlKey: true }), overlaid)).toBe('toggle-pin');
    // And the corresponding Meta presses no longer match — Cmd-shaped
    // combos are not what the user sees on a Windows/Linux keyboard.
    expect(resolveAction(event({ key: 'k', metaKey: true }), overlaid)).toBeUndefined();
  });

  it('keeps Emacs Ctrl+N/Ctrl+P on macOS, drops them on non-mac to avoid collision', () => {
    // On macOS the primary modifier is Cmd, so `Ctrl+P` (Emacs prev) and
    // `Cmd+P` (toggle-pin) are distinct chords. On Win/Linux the primary
    // modifier collapses to Ctrl, so `Ctrl+P` would have to be either
    // select-prev or toggle-pin — keep toggle-pin (the action the user
    // most likely reaches for) and rely on Arrow keys for navigation.
    const macOverlaid = buildBindings({}, 'macos');
    const winOverlaid = buildBindings({}, 'windows');
    expect(resolveAction(event({ key: 'n', ctrlKey: true }), macOverlaid)).toBe('select-next');
    expect(resolveAction(event({ key: 'p', ctrlKey: true }), macOverlaid)).toBe('select-prev');
    expect(resolveAction(event({ key: 'n', ctrlKey: true }), winOverlaid)).toBeUndefined();
    expect(resolveAction(event({ key: 'p', ctrlKey: true }), winOverlaid)).toBe('toggle-pin');
  });
});

describe('isPrimaryModifierHeld', () => {
  it('returns metaKey on macOS and ctrlKey elsewhere', () => {
    expect(isPrimaryModifierHeld({ metaKey: true }, 'macos')).toBe(true);
    expect(isPrimaryModifierHeld({ ctrlKey: true }, 'macos')).toBe(false);
    expect(isPrimaryModifierHeld({ ctrlKey: true }, 'windows')).toBe(true);
    expect(isPrimaryModifierHeld({ metaKey: true }, 'windows')).toBe(false);
    expect(isPrimaryModifierHeld({ ctrlKey: true }, 'linuxWayland')).toBe(true);
  });
});

describe('formatAccelerator', () => {
  it('renders CmdOrCtrl+Shift+V as glyphs on macOS', () => {
    expect(formatAccelerator('CmdOrCtrl+Shift+V', 'macos')).toBe('⇧⌘V');
  });

  it('renders CmdOrCtrl+Shift+V as Ctrl+Shift+V on Windows', () => {
    expect(formatAccelerator('CmdOrCtrl+Shift+V', 'windows')).toBe('Ctrl+Shift+V');
  });

  it('renders CmdOrCtrl+Shift+V as Ctrl+Shift+V on Linux Wayland', () => {
    expect(formatAccelerator('CmdOrCtrl+Shift+V', 'linuxWayland')).toBe('Ctrl+Shift+V');
  });

  it('keeps explicit Cmd as Meta glyph / Win label across platforms', () => {
    // The portable `CmdOrCtrl` alias swaps per host; an explicit `Cmd` token
    // is a deliberate request for the physical Meta/Win key and must not be
    // remapped just because the display platform differs.
    expect(formatAccelerator('Cmd+I', 'macos')).toBe('⌘I');
    expect(formatAccelerator('Cmd+I', 'windows')).toBe('Win+I');
    expect(formatAccelerator('Cmd+I', 'linuxWayland')).toBe('Super+I');
  });

  it('orders mac modifiers Ctrl, Opt, Shift, Cmd to match the Apple HIG', () => {
    expect(formatAccelerator('Cmd+Ctrl+Alt+Shift+K', 'macos')).toBe('⌃⌥⇧⌘K');
  });

  it('renders multi-char keys in their idiomatic per-OS label', () => {
    expect(formatAccelerator('CmdOrCtrl+Backspace', 'macos')).toBe('⌘⌫');
    expect(formatAccelerator('CmdOrCtrl+Backspace', 'windows')).toBe('Ctrl+Backspace');
    expect(formatAccelerator('CmdOrCtrl+Up', 'macos')).toBe('⌘↑');
    expect(formatAccelerator('CmdOrCtrl+ArrowUp', 'macos')).toBe('⌘↑');
    expect(formatAccelerator('CmdOrCtrl+PageDown', 'windows')).toBe('Ctrl+PgDn');
  });

  it('uppercases single-letter keys regardless of stored case', () => {
    expect(formatAccelerator('CmdOrCtrl+v', 'macos')).toBe('⌘V');
    expect(formatAccelerator('CmdOrCtrl+v', 'windows')).toBe('Ctrl+V');
  });

  it('returns the input verbatim for malformed strings', () => {
    // Multiple key segments is a parsing failure; surfacing the raw value
    // lets the user see what is stored rather than wiping the field to a
    // blank cell.
    expect(formatAccelerator('Cmd+A+B', 'macos')).toBe('Cmd+A+B');
  });

  it('returns empty string for empty input', () => {
    expect(formatAccelerator('', 'macos')).toBe('');
  });

  it('defaults to macOS glyph mode when platform is unknown', () => {
    expect(formatAccelerator('CmdOrCtrl+V')).toBe('⌘V');
  });
});

describe('captureFromKeyboardEvent', () => {
  it('folds Cmd on macOS into CmdOrCtrl', () => {
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'v', code: 'KeyV', metaKey: true, shiftKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBe('CmdOrCtrl+Shift+V');
  });

  it('folds Ctrl on Windows into CmdOrCtrl', () => {
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'v', code: 'KeyV', ctrlKey: true, shiftKey: true }),
        'tauri-global',
        'windows',
      ),
    ).toBe('CmdOrCtrl+Shift+V');
  });

  it('preserves non-primary modifier verbatim (Ctrl on macOS)', () => {
    // Cmd+Ctrl is a deliberate, mac-specific combo — folding Cmd to
    // `CmdOrCtrl` is fine, but Ctrl must stay as `Ctrl` so the recorded
    // shortcut keeps firing on the macOS host where it was captured.
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'k', code: 'KeyK', metaKey: true, ctrlKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBe('CmdOrCtrl+Ctrl+K');
  });

  it('returns null for pure-modifier events so the caller keeps recording', () => {
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'Meta', code: 'MetaLeft', metaKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBeNull();
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'Shift', code: 'ShiftLeft', shiftKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBeNull();
  });

  it('emits Tauri-style multi-char tokens for the global target', () => {
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'ArrowUp', code: 'ArrowUp', metaKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBe('CmdOrCtrl+Up');
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'Escape', code: 'Escape', metaKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBe('CmdOrCtrl+Esc');
  });

  it('emits DOM-style multi-char tokens for the palette-binding target', () => {
    // Palette overrides are matched against `KeyboardEvent.key`, so we have
    // to keep the long DOM names — otherwise a recorded `Up` would never
    // fire because `event.key` is `ArrowUp`.
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'ArrowUp', code: 'ArrowUp', metaKey: true }),
        'palette-binding',
        'macos',
      ),
    ).toBe('CmdOrCtrl+ArrowUp');
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'Escape', code: 'Escape', metaKey: true }),
        'palette-binding',
        'macos',
      ),
    ).toBe('CmdOrCtrl+Escape');
  });

  it('captures punctuation via the physical code, not the shifted glyph', () => {
    // Shift+1 reports `event.key === '!'` on US keyboards; we drive off
    // `event.code` so the stored binding stays in terms of the physical
    // key the user pressed.
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: '!', code: 'Digit1', metaKey: true, shiftKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBe('CmdOrCtrl+Shift+1');
  });

  it('palette-binding captures the shifted character so the matcher fires', () => {
    // The in-window matcher compares `binding.key` against `event.key`,
    // and on US layouts `Shift+1` arrives as `event.key === "!"`. Storing
    // the physical `1` here would silently never match. Save the shifted
    // glyph instead so the recorded combo actually triggers.
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: '!', code: 'Digit1', metaKey: true, shiftKey: true }),
        'palette-binding',
        'macos',
      ),
    ).toBe('CmdOrCtrl+Shift+!');
  });

  it('palette-binding records the layout-specific letter via event.key', () => {
    // AZERTY's `KeyA` physical position produces `event.key === "q"`. The
    // matcher uses `event.key`, so storing the physical `A` would also
    // miss. Using `event.key` keeps the recorded combo portable to the
    // matcher on whichever layout the user is on.
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: 'q', code: 'KeyA', metaKey: true }),
        'palette-binding',
        'macos',
      ),
    ).toBe('CmdOrCtrl+q');
  });

  it('returns null when the physical code has no mapped token', () => {
    // `IntlBackslash` is a non-US layout key Tauri's parser does not
    // accept; returning null keeps the recording UX waiting for the next
    // press instead of persisting a value the daemon would reject.
    expect(
      captureFromKeyboardEvent(
        captureEvent({ key: '\\', code: 'IntlBackslash', metaKey: true }),
        'tauri-global',
        'macos',
      ),
    ).toBeNull();
  });
});

describe('defaultPaletteAccelerator', () => {
  it('formats the built-in default in the macOS idiom', () => {
    expect(defaultPaletteAccelerator('pin', 'macos')).toBe('⌘P');
    expect(defaultPaletteAccelerator('delete', 'macos')).toBe('⌘⌫');
    expect(defaultPaletteAccelerator('paste-as-plain', 'macos')).toBe('⇧⌘↩');
    expect(defaultPaletteAccelerator('copy-without-paste', 'macos')).toBe('⌘↩');
    expect(defaultPaletteAccelerator('open-preview', 'macos')).toBe('⌘E');
  });

  it('swaps Cmd for Ctrl on non-macOS hosts', () => {
    expect(defaultPaletteAccelerator('pin', 'windows')).toBe('Ctrl+P');
    expect(defaultPaletteAccelerator('open-preview', 'linuxWayland')).toBe('Ctrl+E');
  });

  it('returns null for actions that ship without a default', () => {
    // `clear` (clear-query) has no entry in PALETTE_BINDINGS, so the editor
    // must render it as "not set" rather than inventing a binding.
    expect(defaultPaletteAccelerator('clear', 'macos')).toBeNull();
    expect(defaultPaletteAccelerator('clear', 'windows')).toBeNull();
  });
});

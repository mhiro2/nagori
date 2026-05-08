import { describe, expect, it } from 'vitest';

import { buildBindings, resolveAction } from './keybindings';

const event = (init: KeyboardEventInit & { key: string }): KeyboardEvent =>
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
});

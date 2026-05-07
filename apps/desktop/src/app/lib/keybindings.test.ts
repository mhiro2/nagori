import { describe, expect, it } from 'vitest';

import { resolveAction } from './keybindings';

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
});

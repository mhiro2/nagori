import { cleanup, fireEvent, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import HotkeyInput from './HotkeyInput.svelte';

afterEach(cleanup);

const defaultProps = {
  platform: 'macos' as const,
  target: 'tauri-global' as const,
  placeholder: 'Click to record',
  recordingLabel: 'Press shortcut',
  clearLabel: 'Clear shortcut',
  onChange: () => {},
};

describe('HotkeyInput', () => {
  it('renders the OS-formatted accelerator when a value is present', () => {
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: 'CmdOrCtrl+Shift+V' },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    // macOS glyphs: Shift then Cmd, then key V.
    expect(display.textContent?.trim()).toBe('⇧⌘V');
  });

  it('switches to Ctrl+Shift+V on non-macOS platforms', () => {
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, platform: 'windows', value: 'CmdOrCtrl+Shift+V' },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    expect(display.textContent?.trim()).toBe('Ctrl+Shift+V');
  });

  it('shows the placeholder when no value is stored', () => {
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: '' },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    expect(display.textContent?.trim()).toBe('Click to record');
  });

  it('enters recording mode on click and shows the recording hint', async () => {
    const user = userEvent.setup();
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: 'CmdOrCtrl+V' },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    await user.click(display);
    expect(display.classList.contains('recording')).toBe(true);
    expect(display.textContent?.trim()).toBe('Press shortcut');
  });

  it('commits a captured combo via onChange and exits recording', async () => {
    const onChange = vi.fn();
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: '', onChange },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    await fireEvent.click(display);
    // Simulate the user pressing Cmd+Shift+P: a non-modifier key with the
    // primary OS modifier and Shift held. The component folds Cmd to
    // `CmdOrCtrl` for portability.
    await fireEvent.keyDown(display, {
      key: 'p',
      code: 'KeyP',
      metaKey: true,
      shiftKey: true,
    });
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith('CmdOrCtrl+Shift+P');
    expect(display.classList.contains('recording')).toBe(false);
  });

  it('ignores pure-modifier presses while recording', async () => {
    const onChange = vi.fn();
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: '', onChange },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    await fireEvent.click(display);
    // Pressing Cmd alone (no non-modifier yet) must not commit; the user
    // is still composing the combo.
    await fireEvent.keyDown(display, { key: 'Meta', code: 'MetaLeft', metaKey: true });
    expect(onChange).not.toHaveBeenCalled();
    expect(display.classList.contains('recording')).toBe(true);
  });

  it('cancels recording on bare Escape without changing the value', async () => {
    const onChange = vi.fn();
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: 'CmdOrCtrl+V', onChange },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    await fireEvent.click(display);
    await fireEvent.keyDown(display, { key: 'Escape', code: 'Escape' });
    expect(onChange).not.toHaveBeenCalled();
    expect(display.classList.contains('recording')).toBe(false);
  });

  it('still commits Escape when modifiers are held', async () => {
    // Bare Esc cancels, but Cmd+Esc is a real shortcut and must commit.
    const onChange = vi.fn();
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: '', onChange },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    await fireEvent.click(display);
    await fireEvent.keyDown(display, {
      key: 'Escape',
      code: 'Escape',
      metaKey: true,
    });
    expect(onChange).toHaveBeenCalledWith('CmdOrCtrl+Esc');
  });

  it('exposes a clear button that wipes the stored value', async () => {
    const onChange = vi.fn();
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: 'CmdOrCtrl+V', onChange },
    });
    const clear = container.querySelector('.clear') as HTMLButtonElement;
    expect(clear).toBeTruthy();
    await fireEvent.click(clear);
    expect(onChange).toHaveBeenCalledWith('');
  });

  it('restores focus to the recording button after clearing the value', async () => {
    // The component is controlled, so the parent normally drops `value` in
    // response to onChange — which unmounts this very trailing button. The fix
    // hands focus back to the persistent recording button *during* the click,
    // so focus is never stranded on <body> regardless of when the prop
    // updates. Asserting focus right after the click captures exactly that.
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: 'CmdOrCtrl+V' },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    const clear = container.querySelector('.clear') as HTMLButtonElement;
    await fireEvent.click(clear);
    expect(document.activeElement).toBe(display);
  });

  it('hides the clear button while recording so the click target is unambiguous', async () => {
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: 'CmdOrCtrl+V' },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    await fireEvent.click(display);
    expect(container.querySelector('.clear')).toBeNull();
  });

  it('preserves a non-primary modifier verbatim when recording', async () => {
    // Cmd+Ctrl on macOS: Cmd is the primary, folds to CmdOrCtrl. Ctrl is
    // not the primary and stays as a literal token so the recorded combo
    // keeps firing on the host where it was captured.
    const onChange = vi.fn();
    const { container } = render(HotkeyInput, {
      props: { ...defaultProps, value: '', onChange },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    await fireEvent.click(display);
    await fireEvent.keyDown(display, {
      key: 'k',
      code: 'KeyK',
      metaKey: true,
      ctrlKey: true,
    });
    expect(onChange).toHaveBeenCalledWith('CmdOrCtrl+Ctrl+K');
  });
});

describe('HotkeyInput — group variants', () => {
  // Group-aware props shared by every variant. `defaultDisplay` is added per
  // test rather than here — under `exactOptionalPropertyTypes` an optional
  // prop can be omitted but not explicitly set to `undefined`, and the
  // secondary variant (like its SettingsTabGeneral call site) omits it.
  const baseProps = {
    platform: 'macos' as const,
    target: 'palette-binding' as const,
    ariaLabel: 'Toggle pin shortcut',
    placeholder: 'Set shortcut',
    recordingLabel: 'Press shortcut…',
    recordingCancelHint: 'Esc to cancel',
    clearLabel: 'Clear shortcut',
    defaultMarker: 'Default',
    disabledMarker: 'Disabled',
    notSet: 'Not set',
    restoreText: 'Reset',
    restoreLabel: 'Restore default for Toggle pin',
    removeLabel: 'Remove shortcut for Toggle pin',
    onChange: () => {},
  };

  it('palette: shows the muted default key + Default marker, no trailing control, when not overridden', () => {
    const { container } = render(HotkeyInput, {
      props: { ...baseProps, variant: 'palette', defaultDisplay: '⌘P', value: '' },
    });
    const combo = container.querySelector('.combo');
    expect(combo?.textContent).toBe('⌘P');
    expect(combo?.classList.contains('muted')).toBe(true);
    expect(container.querySelector('.marker')?.textContent).toBe('Default');
    expect(container.querySelector('.clear')).toBeNull();
  });

  it('palette: shows the custom key (not muted) + a restore-default control when overridden', async () => {
    const onChange = vi.fn();
    const { container } = render(HotkeyInput, {
      props: {
        ...baseProps,
        variant: 'palette',
        defaultDisplay: '⌘P',
        value: 'CmdOrCtrl+Shift+P',
        onChange,
      },
    });
    const combo = container.querySelector('.combo');
    expect(combo?.textContent).toBe('⇧⌘P');
    expect(combo?.classList.contains('muted')).toBe(false);
    expect(container.querySelector('.marker')).toBeNull();
    const reset = container.querySelector('.clear') as HTMLButtonElement;
    expect(reset.getAttribute('aria-label')).toBe('Restore default for Toggle pin');
    // Restore is a short action-word text chip ("Reset"), distinct from the
    // in-field "Default" status marker and not a glyph.
    expect(reset.classList.contains('text-chip')).toBe(true);
    expect(reset.textContent?.trim()).toBe('Reset');
    await fireEvent.click(reset);
    expect(onChange).toHaveBeenCalledWith('');
  });

  it('palette-optional: shows "Not set" with no marker / control when unset', () => {
    const { container } = render(HotkeyInput, {
      props: { ...baseProps, variant: 'palette-optional', defaultDisplay: null, value: '' },
    });
    expect(container.querySelector('.hint')?.textContent).toBe('Not set');
    expect(container.querySelector('.marker')).toBeNull();
    expect(container.querySelector('.clear')).toBeNull();
  });

  it('palette-optional: shows a × remove-shortcut control when set', () => {
    const { container } = render(HotkeyInput, {
      props: {
        ...baseProps,
        variant: 'palette-optional',
        defaultDisplay: null,
        value: 'CmdOrCtrl+Backspace',
      },
    });
    const remove = container.querySelector('.clear') as HTMLButtonElement;
    expect(remove.getAttribute('aria-label')).toBe('Remove shortcut for Toggle pin');
    expect(remove.textContent?.trim()).toBe('×');
  });

  it('secondary: shows "Not set" + a Disabled marker, no control, when unset', () => {
    const { container } = render(HotkeyInput, {
      props: {
        ...baseProps,
        variant: 'secondary',
        ariaLabel: 'Repaste latest item shortcut',
        removeLabel: 'Disable Repaste latest item',
        value: '',
      },
    });
    expect(container.querySelector('.hint')?.textContent).toBe('Not set');
    expect(container.querySelector('.marker')?.textContent).toBe('Disabled');
    expect(container.querySelector('.clear')).toBeNull();
  });

  it('secondary: shows a × disable control when set', () => {
    const { container } = render(HotkeyInput, {
      props: {
        ...baseProps,
        variant: 'secondary',
        ariaLabel: 'Repaste latest item shortcut',
        removeLabel: 'Disable Repaste latest item',
        value: 'CmdOrCtrl+Shift+R',
      },
    });
    const disable = container.querySelector('.clear') as HTMLButtonElement;
    expect(disable.getAttribute('aria-label')).toBe('Disable Repaste latest item');
    expect(disable.textContent?.trim()).toBe('×');
  });

  it('folds the action name and current state into the accessible name', () => {
    const { container } = render(HotkeyInput, {
      props: { ...baseProps, variant: 'palette', defaultDisplay: '⌘P', value: '' },
    });
    const display = container.querySelector('.display') as HTMLButtonElement;
    expect(display.getAttribute('aria-label')).toBe('Toggle pin shortcut, ⌘P, Default');
  });
});

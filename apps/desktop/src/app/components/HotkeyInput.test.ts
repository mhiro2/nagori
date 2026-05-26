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

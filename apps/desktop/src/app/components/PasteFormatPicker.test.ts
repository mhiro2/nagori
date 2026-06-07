import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Spy on the action handlers so the test asserts the picker's wiring, not the
// downstream paste IPC.
vi.mock('../stores/searchActions', () => ({
  confirmPasteFormat: vi.fn(),
  cancelPasteFormat: vi.fn(),
}));

import { closePasteFormatPicker, openPasteFormatPicker } from '../stores/pasteFormatPicker.svelte';
import { cancelPasteFormat, confirmPasteFormat } from '../stores/searchActions';
import PasteFormatPicker from './PasteFormatPicker.svelte';

afterEach(() => {
  cleanup();
  closePasteFormatPicker();
});

beforeEach(() => {
  vi.clearAllMocks();
  openPasteFormatPicker('e1', [
    { mime: 'text/uri-list', category: 'files' },
    { mime: 'image/png', category: 'image' },
  ]);
});

describe('PasteFormatPicker', () => {
  it('renders a keep-original row plus a row per option, with image subtype disambiguated', () => {
    const { getByRole } = render(PasteFormatPicker);
    expect(getByRole('menuitem', { name: 'Keep original format' })).toBeTruthy();
    expect(getByRole('menuitem', { name: 'Files' })).toBeTruthy();
    // Image rows append the concrete subtype so two image formats stay distinct.
    expect(getByRole('menuitem', { name: 'Image (PNG)' })).toBeTruthy();
  });

  it('focuses the first row on open so the keyboard owns the picker', () => {
    const { getByRole } = render(PasteFormatPicker);
    // Synchronous focus on mount — no await — so arrows work without a click.
    expect(document.activeElement).toBe(getByRole('menuitem', { name: 'Keep original format' }));
  });

  it('moves focus between rows with the arrow keys', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(PasteFormatPicker);
    await user.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(getByRole('menuitem', { name: 'Files' }));
    await user.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(getByRole('menuitem', { name: 'Image (PNG)' }));
    await user.keyboard('{ArrowUp}');
    expect(document.activeElement).toBe(getByRole('menuitem', { name: 'Files' }));
  });

  it('keep-original applies the default paste (undefined option)', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(PasteFormatPicker);
    await user.click(getByRole('menuitem', { name: 'Keep original format' }));
    expect(confirmPasteFormat).toHaveBeenCalledWith(undefined);
  });

  it('an option row applies exactly that representation', async () => {
    const user = userEvent.setup();
    const { getByRole } = render(PasteFormatPicker);
    await user.click(getByRole('menuitem', { name: 'Image (PNG)' }));
    expect(confirmPasteFormat).toHaveBeenCalledWith({ mime: 'image/png', category: 'image' });
  });

  it('Escape cancels the picker', async () => {
    const user = userEvent.setup();
    render(PasteFormatPicker);
    await user.keyboard('{Escape}');
    expect(cancelPasteFormat).toHaveBeenCalledTimes(1);
  });
});

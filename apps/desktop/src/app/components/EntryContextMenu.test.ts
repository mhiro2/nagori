import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Spy on the action wrappers so the tests assert the menu's wiring, not the
// downstream copy/paste/delete IPC.
vi.mock('../stores/searchActions', () => ({
  copyEntryById: vi.fn(),
  pasteEntryById: vi.fn(),
  openPasteFormatPickerFor: vi.fn(),
  deleteEntryById: vi.fn(),
  togglePinEntry: vi.fn(),
  copyEntriesByIds: vi.fn(),
  deleteEntriesByIds: vi.fn(),
}));

import {
  closeEntryContextMenu,
  entryContextMenuState,
  openEntryContextMenu,
} from '../stores/entryContextMenu.svelte';
import {
  copyEntriesByIds,
  copyEntryById,
  deleteEntriesByIds,
  deleteEntryById,
  openPasteFormatPickerFor,
  pasteEntryById,
  togglePinEntry,
} from '../stores/searchActions';
import EntryContextMenu from './EntryContextMenu.svelte';

afterEach(() => {
  cleanup();
  closeEntryContextMenu();
});

beforeEach(() => {
  vi.clearAllMocks();
});

describe('EntryContextMenu (single target)', () => {
  beforeEach(() => {
    openEntryContextMenu({
      x: 20,
      y: 30,
      targetIds: ['r1'],
      primaryPinned: false,
      offersFormatChoice: true,
    });
  });

  it('renders the full per-entry surface', () => {
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    for (const key of ['paste', 'copy', 'pasteAs', 'pin', 'actions', 'delete']) {
      expect(getByTestId(`context-menu-${key}`)).toBeTruthy();
    }
  });

  it('focuses the first item on open so the keyboard owns the menu', () => {
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    expect(document.activeElement).toBe(getByTestId('context-menu-paste'));
  });

  it('copy dispatches copyEntryById with the captured id and closes', async () => {
    const user = userEvent.setup();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.click(getByTestId('context-menu-copy'));
    expect(copyEntryById).toHaveBeenCalledWith('r1');
    expect(entryContextMenuState.open).toBe(false);
  });

  it('paste dispatches pasteEntryById with the captured id', async () => {
    const user = userEvent.setup();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.click(getByTestId('context-menu-paste'));
    expect(pasteEntryById).toHaveBeenCalledWith('r1');
  });

  it('paste-as opens the format picker for the captured id', async () => {
    const user = userEvent.setup();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.click(getByTestId('context-menu-pasteAs'));
    expect(openPasteFormatPickerFor).toHaveBeenCalledWith('r1');
  });

  it('delete dispatches deleteEntryById with the captured id', async () => {
    const user = userEvent.setup();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.click(getByTestId('context-menu-delete'));
    expect(deleteEntryById).toHaveBeenCalledWith('r1');
  });

  it('the actions row hands the captured id back to the parent', async () => {
    const user = userEvent.setup();
    const onOpenActions = vi.fn();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions } });
    await user.click(getByTestId('context-menu-actions'));
    expect(onOpenActions).toHaveBeenCalledWith('r1');
    expect(entryContextMenuState.open).toBe(false);
  });

  it('pin toggles from the captured pinned state', async () => {
    const user = userEvent.setup();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.click(getByTestId('context-menu-pin'));
    expect(togglePinEntry).toHaveBeenCalledWith({ id: 'r1', pinned: false });
  });

  it('Escape closes the menu without hiding the palette', async () => {
    const user = userEvent.setup();
    render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.keyboard('{Escape}');
    expect(entryContextMenuState.open).toBe(false);
  });
});

describe('EntryContextMenu (already-pinned single target)', () => {
  it('labels the pin row Unpin', () => {
    openEntryContextMenu({
      x: 0,
      y: 0,
      targetIds: ['r1'],
      primaryPinned: true,
      offersFormatChoice: false,
    });
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    expect(getByTestId('context-menu-pin').textContent?.trim()).toBe('Unpin');
  });
});

describe('EntryContextMenu (single target without a format choice)', () => {
  beforeEach(() => {
    openEntryContextMenu({
      x: 0,
      y: 0,
      targetIds: ['r1'],
      primaryPinned: false,
      offersFormatChoice: false,
    });
  });

  it('omits the paste-as row when the entry has only one pasteable format', () => {
    const { getByTestId, queryByTestId } = render(EntryContextMenu, {
      props: { onOpenActions: vi.fn() },
    });
    // The rest of the single-target surface still renders…
    expect(getByTestId('context-menu-paste')).toBeTruthy();
    expect(getByTestId('context-menu-copy')).toBeTruthy();
    expect(getByTestId('context-menu-actions')).toBeTruthy();
    expect(getByTestId('context-menu-delete')).toBeTruthy();
    // …but paste-as is hidden since it would have no real choice to offer.
    expect(queryByTestId('context-menu-pasteAs')).toBeNull();
  });
});

describe('EntryContextMenu (multi target)', () => {
  beforeEach(() => {
    openEntryContextMenu({
      x: 0,
      y: 0,
      targetIds: ['a', 'b', 'c'],
      primaryPinned: false,
      offersFormatChoice: false,
    });
  });

  it('reduces to combined copy and bulk delete only', () => {
    const { getByTestId, queryByTestId } = render(EntryContextMenu, {
      props: { onOpenActions: vi.fn() },
    });
    expect(getByTestId('context-menu-copy')).toBeTruthy();
    expect(getByTestId('context-menu-delete')).toBeTruthy();
    // Per-entry rows make no sense for a multi-selection.
    expect(queryByTestId('context-menu-paste')).toBeNull();
    expect(queryByTestId('context-menu-pasteAs')).toBeNull();
    expect(queryByTestId('context-menu-pin')).toBeNull();
    expect(queryByTestId('context-menu-actions')).toBeNull();
  });

  it('copy routes to the combined-copy bulk action with the captured ids', async () => {
    const user = userEvent.setup();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.click(getByTestId('context-menu-copy'));
    expect(copyEntriesByIds).toHaveBeenCalledWith(['a', 'b', 'c']);
  });

  it('delete routes to the bulk delete action with the captured ids', async () => {
    const user = userEvent.setup();
    const { getByTestId } = render(EntryContextMenu, { props: { onOpenActions: vi.fn() } });
    await user.click(getByTestId('context-menu-delete'));
    expect(deleteEntriesByIds).toHaveBeenCalledWith(['a', 'b', 'c']);
  });
});

import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => false),
}));

vi.mock('../lib/commands', () => ({
  closePalette: vi.fn(async () => undefined),
  // PreviewPane / OnboardingBanner / settings store reach the same module.
  openAccessibilitySettings: vi.fn(),
  getEntryPreview: vi.fn(),
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
  searchClipboard: vi.fn(),
  listRecent: vi.fn(async () => []),
  pasteEntryFromPalette: vi.fn(),
  copyEntryFromPalette: vi.fn(),
  pinEntry: vi.fn(),
  deleteEntry: vi.fn(),
}));

vi.mock('../stores/searchActions', () => ({
  confirmSelection: vi.fn(async () => undefined),
  confirmSelectionWithAlternateFormat: vi.fn(async () => undefined),
  copySelection: vi.fn(async () => undefined),
  togglePinSelection: vi.fn(async () => undefined),
  deleteSelection: vi.fn(async () => undefined),
}));

vi.mock('../stores/searchPreview.svelte', () => ({
  hydratePreview: vi.fn(async () => undefined),
  previewState: {
    entryId: undefined,
    preview: undefined,
    loading: false,
    errorMessage: undefined,
  },
}));

vi.mock('../stores/searchQuery.svelte', () => ({
  refreshRecent: vi.fn(async () => undefined),
  scheduleQuery: vi.fn(),
  searchState: {
    query: '',
    results: [],
    selectedIndex: 0,
    loading: false,
    errorMessage: undefined,
    lastElapsedMs: undefined,
  },
}));

vi.mock('../stores/searchSelection', () => ({
  currentSelection: vi.fn(() => undefined),
  selectByIndex: vi.fn(),
  selectFirst: vi.fn(),
  selectLast: vi.fn(),
  selectNext: vi.fn(),
  selectPrev: vi.fn(),
}));

vi.mock('../stores/settings.svelte', () => ({
  refreshSettings: vi.fn(async () => undefined),
  settingsState: {
    settings: undefined,
    permissions: [],
    loaded: true,
    errorMessage: undefined,
  },
  captureEnabled: () => true,
  aiEnabled: () => false,
  accessibilityState: () => undefined,
  accessibilityGranted: () => true,
}));

vi.mock('../stores/view.svelte', () => ({
  showSettings: vi.fn(),
  showPalette: vi.fn(),
  viewState: { current: 'palette' as const },
}));

import { closePalette } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import {
  confirmSelection,
  confirmSelectionWithAlternateFormat,
  copySelection,
  deleteSelection,
  togglePinSelection,
} from '../stores/searchActions';
import { scheduleQuery } from '../stores/searchQuery.svelte';
import { selectFirst, selectLast, selectNext, selectPrev } from '../stores/searchSelection';
import { showSettings } from '../stores/view.svelte';
import Palette from './Palette.svelte';

const dispatch = (init: KeyboardEventInit): KeyboardEvent => {
  const event = new KeyboardEvent('keydown', { ...init, bubbles: true, cancelable: true });
  return event;
};

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(cleanup);

describe('Palette', () => {
  it('renders the palette frame with the search box', () => {
    const { container } = render(Palette);
    expect(container.querySelector('.palette')).toBeTruthy();
    expect(container.querySelector('input[type="text"]')).toBeTruthy();
  });

  it('forwards search input to scheduleQuery', async () => {
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    expect(input).toBeTruthy();
    if (input) {
      await fireEvent.input(input, { target: { value: 'q' } });
    }
    expect(scheduleQuery).toHaveBeenCalledWith('q');
  });

  // The keybinding contract is:
  //   ↓ → selectNext, ↑ → selectPrev, Home → selectFirst, End → selectLast,
  //   Enter → confirm, Cmd+Enter → copy, Cmd+P → toggle-pin,
  //   Cmd+Backspace → delete, Cmd+, → settings, Esc → close.
  // Each row asserts one binding to keep regressions easy to localise.
  const cases: Array<{
    name: string;
    init: KeyboardEventInit;
    spy: () => void;
  }> = [
    {
      name: 'ArrowDown selects next',
      init: { key: 'ArrowDown' },
      spy: () => expect(selectNext).toHaveBeenCalled(),
    },
    {
      name: 'ArrowUp selects prev',
      init: { key: 'ArrowUp' },
      spy: () => expect(selectPrev).toHaveBeenCalled(),
    },
    {
      name: 'Home selects first',
      init: { key: 'Home' },
      spy: () => expect(selectFirst).toHaveBeenCalled(),
    },
    {
      name: 'End selects last',
      init: { key: 'End' },
      spy: () => expect(selectLast).toHaveBeenCalled(),
    },
    {
      name: 'Enter confirms',
      init: { key: 'Enter' },
      spy: () => expect(confirmSelection).toHaveBeenCalled(),
    },
    {
      name: 'Cmd+Enter copies',
      init: { key: 'Enter', metaKey: true },
      spy: () => expect(copySelection).toHaveBeenCalled(),
    },
    {
      name: 'Cmd+Shift+Enter confirms with alternate format',
      init: { key: 'Enter', metaKey: true, shiftKey: true },
      spy: () => expect(confirmSelectionWithAlternateFormat).toHaveBeenCalled(),
    },
    {
      name: 'Cmd+P toggles pin',
      init: { key: 'p', metaKey: true },
      spy: () => expect(togglePinSelection).toHaveBeenCalled(),
    },
    {
      name: 'Cmd+Backspace deletes',
      init: { key: 'Backspace', metaKey: true },
      spy: () => expect(deleteSelection).toHaveBeenCalled(),
    },
    {
      name: 'Cmd+, opens settings',
      init: { key: ',', metaKey: true },
      spy: () => expect(showSettings).toHaveBeenCalled(),
    },
  ];

  for (const { name, init, spy } of cases) {
    it(name, async () => {
      const { container } = render(Palette);
      const input = container.querySelector('input[type="text"]');
      if (input) await fireEvent.keyDown(input, init);
      spy();
    });
  }

  it('closes the palette on Escape inside the Tauri runtime', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'Escape' });
    expect(closePalette).toHaveBeenCalled();
  });

  it('does not call closePalette outside Tauri', async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'Escape' });
    expect(closePalette).not.toHaveBeenCalled();
  });

  it('ignores keys without a binding', async () => {
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'q' });
    expect(selectNext).not.toHaveBeenCalled();
    expect(confirmSelection).not.toHaveBeenCalled();
  });

  it('opens the action menu on Cmd+K', async () => {
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    // Action menu is rendered via the same container; opening flips a local
    // state and renders a [role="dialog"].
    expect(container.querySelector('[role="dialog"]')).toBeTruthy();
  });
});

// `dispatch` is currently unused but kept here as a convenience for future
// tests that need to bypass `fireEvent` and own KeyboardEvent construction.
void dispatch;

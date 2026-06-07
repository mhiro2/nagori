import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => false),
  subscribe: vi.fn(() => () => undefined),
  TAURI_EVENTS: {
    clipboardChanged: 'nagori://clipboard_changed',
  },
}));

vi.mock('../lib/commands', () => ({
  closePalette: vi.fn(async () => undefined),
  openSettingsWindow: vi.fn(async () => undefined),
  // PreviewPane / settings store reach the same module.
  requestAccessibility: vi.fn(async () => ({ kind: 'accessibility', state: 'granted' })),
  getEntryPreview: vi.fn(),
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
  searchClipboard: vi.fn(),
  listRecent: vi.fn(async () => []),
  pasteEntryFromPalette: vi.fn(),
  copyEntryFromPalette: vi.fn(),
  pinEntry: vi.fn(),
  deleteEntry: vi.fn(),
  previewEntry: vi.fn(async () => undefined),
  getCapabilities: vi.fn(),
}));

vi.mock('../stores/searchActions', () => ({
  confirmSelection: vi.fn(async () => undefined),
  confirmSelectionWithAlternateFormat: vi.fn(async () => undefined),
  copySelection: vi.fn(async () => undefined),
  togglePinSelection: vi.fn(async () => undefined),
  togglePinAt: vi.fn(async () => undefined),
  deleteSelection: vi.fn(async () => undefined),
  previewSelection: vi.fn(async () => undefined),
}));

vi.mock('../stores/capabilities.svelte', () => ({
  capabilitiesState: { capabilities: undefined, loaded: true },
  refreshCapabilities: vi.fn(async () => undefined),
  quickLookAvailable: vi.fn(() => true),
  // The action inspector (rendered by the palette) gates its AI actions on
  // this. Default false — matching the real helper when no capability
  // snapshot is loaded — so these tests exercise the quick-action path.
  aiActionsSupported: vi.fn(() => false),
}));

vi.mock('../stores/searchPreview.svelte', () => ({
  hydratePreview: vi.fn(async () => undefined),
  expandPreview: vi.fn(async () => undefined),
  previewState: {
    entryId: undefined,
    preview: undefined,
    loading: false,
    loadingVisible: false,
    errorMessage: undefined,
    expandedLoading: false,
    expandedErrorMessage: undefined,
  },
}));

vi.mock('../stores/searchQuery.svelte', () => ({
  refreshCurrent: vi.fn(async () => undefined),
  refreshRecent: vi.fn(async () => undefined),
  scheduleQuery: vi.fn(),
  cancelPendingQuery: vi.fn(),
  searchState: {
    query: '',
    appliedQuery: '',
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

import { closePalette, openSettingsWindow } from '../lib/commands';
import { isTauri, subscribe, TAURI_EVENTS } from '../lib/tauri';
import type { EntryPreviewDto, PlatformCapabilities, SearchResultDto } from '../lib/types';
import { capabilitiesState, quickLookAvailable } from '../stores/capabilities.svelte';
import {
  confirmSelection,
  confirmSelectionWithAlternateFormat,
  copySelection,
  deleteSelection,
  previewSelection,
  togglePinAt,
  togglePinSelection,
} from '../stores/searchActions';
import { previewState } from '../stores/searchPreview.svelte';
import { refreshCurrent, scheduleQuery, searchState } from '../stores/searchQuery.svelte';
import {
  currentSelection,
  selectByIndex,
  selectFirst,
  selectLast,
  selectNext,
  selectPrev,
} from '../stores/searchSelection';
import { settingsState } from '../stores/settings.svelte';
import { showSettings } from '../stores/view.svelte';
import Palette from './Palette.svelte';

const dispatch = (init: KeyboardEventInit): KeyboardEvent => {
  const event = new KeyboardEvent('keydown', { ...init, bubbles: true, cancelable: true });
  return event;
};

const resultRow = (id: string, snippet: string): SearchResultDto => ({
  id,
  kind: 'text',
  preview: snippet,
  score: 1,
  createdAt: '2026-05-27T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: [],
  representationSummary: [],
});

const textPreview = (id: string, body: string): EntryPreviewDto => ({
  id,
  kind: 'text',
  title: 'T',
  previewText: body,
  body: { type: 'text', text: body },
  metadata: {
    byteCount: body.length,
    charCount: body.length,
    lineCount: 1,
    truncated: false,
    sensitive: false,
    fullContentAvailable: true,
  },
});

const urlRow = (id: string, url: string): SearchResultDto => ({
  id,
  kind: 'url',
  preview: url,
  score: 1,
  createdAt: '2026-05-27T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: [],
  representationSummary: [],
});

const urlPreview = (id: string, url: string): EntryPreviewDto => ({
  id,
  kind: 'url',
  title: 'U',
  previewText: url,
  body: {
    type: 'url',
    url,
    domain: 'example.com',
    scheme: 'https',
    hostDisplay: 'example.com',
    pathAndQuery: '/',
  },
  metadata: {
    byteCount: url.length,
    charCount: url.length,
    lineCount: 1,
    truncated: false,
    sensitive: false,
    fullContentAvailable: true,
    domain: 'example.com',
  },
});

beforeEach(() => {
  vi.clearAllMocks();
  // `vi.clearAllMocks` wipes call history but keeps any `mockReturnValue`
  // implementation a prior test installed, so re-pin the defaults the
  // selection-dependent tests below override per-case.
  vi.mocked(currentSelection).mockReturnValue(undefined);
  previewState.entryId = undefined;
  previewState.preview = undefined;
  previewState.loading = false;
  previewState.loadingVisible = false;
  previewState.errorMessage = undefined;
  searchState.results = [];
  searchState.selectedIndex = 0;
  settingsState.settings = undefined;
  capabilitiesState.capabilities = undefined;
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

  it('refreshes the active query when capture stores a new entry', () => {
    let handler: ((payload: { entryId: string }) => void) | undefined;
    vi.mocked(subscribe).mockImplementation((event, next) => {
      if (event === TAURI_EVENTS.clipboardChanged) {
        handler = next as (payload: { entryId: string }) => void;
      }
      return () => undefined;
    });

    render(Palette);
    handler?.({ entryId: 'entry-id' });

    expect(refreshCurrent).toHaveBeenCalled();
  });

  it('backfills via onReady so emits during attach are not lost', () => {
    let onReady: (() => void) | undefined;
    vi.mocked(subscribe).mockImplementation((event, _next, ready) => {
      if (event === TAURI_EVENTS.clipboardChanged) {
        onReady = ready;
      }
      return () => undefined;
    });

    render(Palette);
    vi.mocked(refreshCurrent).mockClear();
    onReady?.();

    expect(refreshCurrent).toHaveBeenCalled();
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

  it('suppresses Enter-to-paste while an expanded URL preview owns Enter', async () => {
    // Regression: a plain Enter in the expanded URL preview must open the URL
    // (handled inside PreviewPane) without *also* pasting the entry. PreviewPane
    // reports `enterOpensUrl`, and the palette stands its confirm binding down.
    const item = urlRow('u1', 'https://example.com/');
    vi.mocked(currentSelection).mockReturnValue(item);
    searchState.results = [item];
    previewState.entryId = 'u1';
    previewState.preview = urlPreview('u1', 'https://example.com/');
    // Remap `open-preview` to a plain key (its default is Cmd/Ctrl+E) so the
    // test can toggle the expanded pane with a single unmodified keystroke.
    settingsState.settings = {
      showPreviewPane: true,
      paletteRowCount: 8,
      paletteHotkeys: { 'open-preview': 'e' },
    } as unknown as NonNullable<typeof settingsState.settings>;

    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    expect(input).toBeTruthy();
    // Expand the preview; PreviewPane mounts and reports `enterOpensUrl`.
    if (input) await fireEvent.keyDown(input, { key: 'e' });
    await tick();
    // Plain Enter now belongs to the URL preview — the palette must not paste.
    if (input) await fireEvent.keyDown(input, { key: 'Enter' });
    expect(confirmSelection).not.toHaveBeenCalled();
  });

  it('lets the IME keep the Enter that commits a 変換 instead of pasting', async () => {
    // Regression: with a Japanese IME, the Enter that confirms a candidate
    // conversion arrives as a keydown flagged `isComposing`. The palette must
    // stand down so the input commits the text rather than pasting the entry.
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    // Assert the target exists so this negative test can't pass vacuously if
    // the selector ever stops matching and no keydown is dispatched.
    expect(input).not.toBeNull();
    await fireEvent.keyDown(input!, { key: 'Enter', isComposing: true });
    expect(confirmSelection).not.toHaveBeenCalled();
  });

  it('does not close the palette when Escape cancels an IME conversion', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    expect(input).not.toBeNull();
    await fireEvent.keyDown(input!, { key: 'Escape', isComposing: true });
    expect(closePalette).not.toHaveBeenCalled();
  });

  it('opens the standalone settings window on Cmd+, inside the Tauri runtime', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    // Pin the platform: once `isTauri()` reports true, `paletteBindingsFor`
    // consults `navigator.userAgent` as a pre-hydration hint. On Linux/Windows
    // CI runners jsdom's UA matches `Linux`, which swaps Cmd+, to Ctrl+, and
    // the metaKey event below would no longer hit the binding.
    capabilitiesState.capabilities = { platform: 'macos' } as PlatformCapabilities;
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: ',', metaKey: true });
    expect(openSettingsWindow).toHaveBeenCalled();
    expect(showSettings).not.toHaveBeenCalled();
  });

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

  it('docks the action inspector on Cmd+K', async () => {
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeNull();
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    // The inspector is a docked panel, not a modal: opening flips a local
    // state and mounts it into the palette body's right column.
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeTruthy();
  });

  it('gives the inspector the right column over the preview pane, then restores it', async () => {
    const item = resultRow('r1', 'snippet');
    vi.mocked(currentSelection).mockReturnValue(item);
    searchState.results = [item];
    settingsState.settings = {
      showPreviewPane: true,
      paletteRowCount: 8,
      paletteHotkeys: {},
    } as unknown as NonNullable<typeof settingsState.settings>;

    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    // Idle: preview pane owns the right column, inspector is unmounted.
    expect(container.querySelector('.preview-pane')).toBeTruthy();
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeNull();

    // Open actions: the inspector takes the slot and the preview steps aside.
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeTruthy();
    expect(container.querySelector('.preview-pane')).toBeNull();

    // Escape closes the inspector and the preview pane comes back.
    if (input) await fireEvent.keyDown(input, { key: 'Escape' });
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeNull();
    expect(container.querySelector('.preview-pane')).toBeTruthy();
  });

  it('toggles the inspector closed on a second Cmd+K', async () => {
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeTruthy();
    // Same chord again dismisses it (the window handler owns the toggle when
    // focus is outside the panel).
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeNull();
  });

  it('opens the inspector from the status-bar actions button', async () => {
    const { container } = render(Palette);
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeNull();
    const btn = container.querySelector('[data-testid="status-open-actions"]');
    expect(btn).toBeTruthy();
    await fireEvent.click(btn as HTMLElement);
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeTruthy();
  });

  it('opens settings from the status-bar settings button', async () => {
    const { container } = render(Palette);
    const btn = container.querySelector('[data-testid="status-open-settings"]');
    expect(btn).toBeTruthy();
    // Non-Tauri test context falls back to the in-process view toggle.
    await fireEvent.click(btn as HTMLElement);
    expect(showSettings).toHaveBeenCalled();
  });

  it('freezes hover selection while the action inspector is open', async () => {
    // Regression: with the inspector docked there is no preview pane for a
    // hovered row to feed, so hover-to-select would only silently re-target the
    // inspector — cancelling an in-flight run or discarding a finished result
    // as the cursor crossed the list. Hover must be inert while it is open.
    const a = resultRow('a', 'alpha');
    const b = resultRow('b', 'bravo');
    vi.mocked(currentSelection).mockReturnValue(a);
    searchState.results = [a, b];
    settingsState.settings = {
      showPreviewPane: true,
      paletteRowCount: 8,
      paletteHotkeys: {},
    } as unknown as NonNullable<typeof settingsState.settings>;

    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeTruthy();

    vi.mocked(selectByIndex).mockClear();
    const rows = container.querySelectorAll('[role="option"]');
    expect(rows.length).toBe(2);
    await fireEvent.mouseEnter(rows[1] as Element);
    expect(selectByIndex).not.toHaveBeenCalled();
  });

  it('selects on hover again once the inspector is closed', async () => {
    const a = resultRow('a', 'alpha');
    const b = resultRow('b', 'bravo');
    vi.mocked(currentSelection).mockReturnValue(a);
    searchState.results = [a, b];

    const { container } = render(Palette);
    const rows = container.querySelectorAll('[role="option"]');
    vi.mocked(selectByIndex).mockClear();
    await fireEvent.mouseEnter(rows[1] as Element);
    expect(selectByIndex).toHaveBeenCalledWith(1);
  });

  it('still re-targets with the arrow keys while the inspector is open', async () => {
    // ↑/↓ stay live as the deliberate re-target path (cancelling the run is the
    // expected cost of an intentional move); only passive hover is frozen.
    const a = resultRow('a', 'alpha');
    vi.mocked(currentSelection).mockReturnValue(a);
    searchState.results = [a, resultRow('b', 'bravo')];

    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    vi.mocked(selectNext).mockClear();
    if (input) await fireEvent.keyDown(input, { key: 'ArrowDown' });
    expect(selectNext).toHaveBeenCalled();
  });

  it('puts the result list into reference mode while the inspector is open', async () => {
    const a = resultRow('a', 'alpha');
    vi.mocked(currentSelection).mockReturnValue(a);
    searchState.results = [a, resultRow('b', 'bravo')];
    settingsState.settings = {
      showPreviewPane: true,
      paletteRowCount: 8,
      paletteHotkeys: {},
    } as unknown as NonNullable<typeof settingsState.settings>;

    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    // Closed: rows are live, not locked.
    expect(container.querySelector('.result-row.locked')).toBeNull();
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });
    // Open: every row carries the locked class so the recede/lift applies.
    expect(container.querySelectorAll('.result-row.locked').length).toBe(2);
  });

  it('ignores list clicks while the inspector is open (no paste, no re-target)', async () => {
    // The list is a read-only reference surface while the inspector owns the
    // column: a click must not paste-and-dismiss (tearing down the palette) nor
    // re-target (cancelling a run). Both plain and modifier clicks are inert.
    const a = resultRow('a', 'alpha');
    const b = resultRow('b', 'bravo');
    vi.mocked(currentSelection).mockReturnValue(a);
    searchState.results = [a, b];
    capabilitiesState.capabilities = { platform: 'macos' } as PlatformCapabilities;
    settingsState.settings = {
      showPreviewPane: true,
      paletteRowCount: 8,
      paletteHotkeys: {},
    } as unknown as NonNullable<typeof settingsState.settings>;

    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });

    vi.mocked(selectByIndex).mockClear();
    const rows = container.querySelectorAll('[role="option"]');
    await fireEvent.click(rows[1] as Element);
    await fireEvent.click(rows[1] as Element, { metaKey: true });
    expect(selectByIndex).not.toHaveBeenCalled();
    expect(confirmSelection).not.toHaveBeenCalled();
  });

  it('makes per-row pin toggles inert while the inspector is open', async () => {
    // `togglePinAt` refreshes via `runQuery`, which publishes a transient
    // `selectedIndex = 0` before re-anchoring by id — observable across the
    // await — so even pinning the target row would briefly re-target and cancel
    // the run. The whole per-row pin affordance stands down while open.
    const a = resultRow('a', 'alpha');
    const b = resultRow('b', 'bravo');
    vi.mocked(currentSelection).mockReturnValue(a);
    searchState.results = [a, b];
    searchState.selectedIndex = 0;
    settingsState.settings = {
      showPreviewPane: true,
      paletteRowCount: 8,
      paletteHotkeys: {},
    } as unknown as NonNullable<typeof settingsState.settings>;

    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'k', metaKey: true });

    const pins = container.querySelectorAll('.pin-toggle');
    expect(pins.length).toBe(2);
    await fireEvent.click(pins[0] as Element);
    await fireEvent.click(pins[1] as Element);
    expect(togglePinAt).not.toHaveBeenCalled();
  });

  it('toggles a pin from its row button while the inspector is closed', async () => {
    const a = resultRow('a', 'alpha');
    const b = resultRow('b', 'bravo');
    vi.mocked(currentSelection).mockReturnValue(a);
    searchState.results = [a, b];

    const { container } = render(Palette);
    const pins = container.querySelectorAll('.pin-toggle');
    await fireEvent.click(pins[1] as Element);
    expect(togglePinAt).toHaveBeenCalledWith(1);
  });

  it('opens the inspector from the preview-pane actions button', async () => {
    const item = resultRow('r1', 'snippet');
    vi.mocked(currentSelection).mockReturnValue(item);
    searchState.results = [item];
    settingsState.settings = {
      showPreviewPane: true,
      paletteRowCount: 8,
      paletteHotkeys: {},
    } as unknown as NonNullable<typeof settingsState.settings>;

    const { container } = render(Palette);
    const btn = container.querySelector('[data-testid="preview-open-actions"]');
    expect(btn).toBeTruthy();
    await fireEvent.click(btn as HTMLElement);
    // The inspector takes the right column and the preview steps aside.
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeTruthy();
    expect(container.querySelector('.preview-pane')).toBeNull();
  });

  it('triggers Quick Look on Cmd+Y when the capability is available', async () => {
    vi.mocked(quickLookAvailable).mockReturnValue(true);
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'y', metaKey: true });
    expect(previewSelection).toHaveBeenCalled();
  });

  it('does not trigger Quick Look when the capability is unavailable', async () => {
    vi.mocked(quickLookAvailable).mockReturnValue(false);
    const { container } = render(Palette);
    const input = container.querySelector('input[type="text"]');
    if (input) await fireEvent.keyDown(input, { key: 'y', metaKey: true });
    expect(previewSelection).not.toHaveBeenCalled();
  });

  // Stale-preview guard: the hydrate is debounced, so right after an arrow
  // press `previewState` still holds the *previously* selected row's body.
  // The pane must not paint that body against the freshly-selected row —
  // most jarringly a clip whose text is the status-bar "⚠ Auto-paste off"
  // warning. Until the store's `entryId` catches up to the live selection
  // the pane falls back to the selected row's own list snippet.
  it('falls back to the row snippet while the store still holds the prior entry', () => {
    const selected = resultRow('sel', 'SELECTED SNIPPET');
    searchState.results = [selected];
    vi.mocked(currentSelection).mockReturnValue(selected);
    // Store lags one entry behind: a different id with the warning body.
    previewState.entryId = 'other';
    previewState.preview = textPreview('other', '⚠ Auto-paste off — Accessibility not granted');

    const { container } = render(Palette);
    const body = container.querySelector('.preview-pane .body');
    expect(body?.textContent).toBe('SELECTED SNIPPET');
    expect(container.textContent).not.toContain('Auto-paste off');
  });

  it('renders the fetched body once the store matches the live selection', () => {
    const selected = resultRow('sel', 'SELECTED SNIPPET');
    searchState.results = [selected];
    vi.mocked(currentSelection).mockReturnValue(selected);
    previewState.entryId = 'sel';
    previewState.preview = textPreview('sel', 'FETCHED BODY');

    const { container } = render(Palette);
    const body = container.querySelector('.preview-pane .body');
    expect(body?.textContent).toBe('FETCHED BODY');
  });
});

// `dispatch` is currently unused but kept here as a convenience for future
// tests that need to bypass `fireEvent` and own KeyboardEvent construction.
void dispatch;

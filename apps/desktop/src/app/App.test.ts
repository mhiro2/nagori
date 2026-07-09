import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./lib/tauri', () => ({
  isTauri: vi.fn(() => false),
  currentWindowLabel: vi.fn(() => undefined),
  subscribe: vi.fn((_event, _handler, onReady) => {
    onReady?.();
    return () => {};
  }),
  TAURI_EVENTS: {
    navigate: 'nagori://navigate',
    pasteFailed: 'nagori://paste_failed',
    captureSkipped: 'nagori://capture_skipped',
    hotkeyRegisterFailed: 'nagori://hotkey_register_failed',
    hotkeyRegisterResolved: 'nagori://hotkey_register_resolved',
    settingsChanged: 'nagori://settings_changed',
  },
}));

vi.mock('./lib/commands', () => ({
  hidePalette: vi.fn(async () => undefined),
  openSettingsWindow: vi.fn(async () => undefined),
  lastHotkeyFailure: vi.fn(async () => null),
  getSettings: vi.fn(async () => undefined),
  getPermissions: vi.fn(async () => []),
}));

// The App shell wires keybindings + window blur; the route children are out
// of scope for these tests and bring their own DOM dependencies, so stub the
// route components down to inert anchors.
vi.mock('./routes/PaletteRoute.svelte', async () => {
  const Stub = (await import('./test-helpers/StubComponent.svelte')).default;
  return { default: Stub };
});

vi.mock('./routes/SettingsRoute.svelte', async () => {
  const Stub = (await import('./test-helpers/StubComponent.svelte')).default;
  return { default: Stub };
});

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

// The palette re-issues the current query when a `settings_changed` event
// flips `recentOrder` (the sort order is applied backend-side as a search
// runs, so the visible list otherwise keeps its old order). Mock the store so
// the tests can assert the re-query without driving a real search.
vi.mock('./stores/searchQuery.svelte', () => ({
  cancelPendingQuery: vi.fn(),
  refreshCurrent: vi.fn(async () => undefined),
}));

import App from './App.svelte';
import { getPermissions, hidePalette, lastHotkeyFailure } from './lib/commands';
import { isTauri, subscribe } from './lib/tauri';
import type { AppSettings, PermissionStatus } from './lib/types';
import { captureSkippedState, clearCaptureSkip } from './stores/captureSkipped.svelte';
import {
  closeEntryContextMenu,
  entryContextMenuState,
  openEntryContextMenu,
} from './stores/entryContextMenu.svelte';
import { dismissHotkeyFailure, hotkeyFailureState } from './stores/hotkeyFailure.svelte';
import { clearPasteDiagnostics, pasteDiagnosticsState } from './stores/pasteDiagnostics.svelte';
import { refreshCurrent } from './stores/searchQuery.svelte';
import { settingsState } from './stores/settings.svelte';
import { showPalette, showSettings, viewState } from './stores/view.svelte';

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(false);
  vi.mocked(lastHotkeyFailure).mockResolvedValue(null);
  vi.mocked(subscribe).mockImplementation((_event, _handler, onReady) => {
    onReady?.();
    return () => {};
  });
  settingsState.settings = undefined;
  settingsState.permissions = [];
  settingsState.permissionsErrorMessage = undefined;
  settingsState.loaded = false;
  dismissHotkeyFailure();
  clearPasteDiagnostics();
  clearCaptureSkip();
  showPalette();
});

afterEach(cleanup);

// Route the `subscribe` mock so a test can fire a
// `nagori://hotkey_register_failed` event into the App-level listener.
// Mirrors the pattern in SettingsView.test.ts; other event subscriptions
// stay wired to a noop unless a test cares about them.
const captureHotkeyFailureHandler = (): {
  fire: (payload: { hotkey: string; error: string; kind?: string; action?: string }) => void;
  fireResolved: (payload: { kind?: string; action?: string }) => void;
} => {
  const failedSlot: {
    handler?: (payload: { hotkey: string; error: string; kind?: string; action?: string }) => void;
  } = {};
  const resolvedSlot: { handler?: (payload: { kind?: string; action?: string }) => void } = {};
  vi.mocked(subscribe).mockImplementation((event, handler, onReady) => {
    if (event === 'nagori://hotkey_register_failed') {
      failedSlot.handler = handler as (payload: {
        hotkey: string;
        error: string;
        kind?: string;
        action?: string;
      }) => void;
    } else if (event === 'nagori://hotkey_register_resolved') {
      resolvedSlot.handler = handler as (payload: { kind?: string; action?: string }) => void;
    }
    onReady?.();
    return () => {};
  });
  return {
    fire: (payload) => {
      if (!failedSlot.handler) throw new Error('hotkey_register_failed handler not registered');
      failedSlot.handler(payload);
    },
    fireResolved: (payload) => {
      if (!resolvedSlot.handler) throw new Error('hotkey_register_resolved handler not registered');
      resolvedSlot.handler(payload);
    },
  };
};

// Capture the `nagori://paste_failed` handler so a test can fire a paste
// failure into App's palette-window listener.
type PasteFailedPayload = { error?: string; reason?: string; tool?: string };
const capturePasteFailedHandler = (): { fire: (payload: PasteFailedPayload) => void } => {
  const slot: { handler?: (payload: PasteFailedPayload) => void } = {};
  vi.mocked(subscribe).mockImplementation((event, handler, onReady) => {
    if (event === 'nagori://paste_failed') {
      slot.handler = handler as (payload: PasteFailedPayload) => void;
    }
    onReady?.();
    return () => {};
  });
  return {
    fire: (payload) => {
      if (!slot.handler) throw new Error('paste_failed handler not registered');
      slot.handler(payload);
    },
  };
};

// Capture the `nagori://settings_changed` handler so a test can fire a
// backend-published settings snapshot into App's palette-window listener.
const captureSettingsChangedHandler = (): { fire: (payload: AppSettings) => void } => {
  const slot: { handler?: (payload: AppSettings) => void } = {};
  vi.mocked(subscribe).mockImplementation((event, handler, onReady) => {
    if (event === 'nagori://settings_changed') {
      slot.handler = handler as (payload: AppSettings) => void;
    }
    onReady?.();
    return () => {};
  });
  return {
    fire: (payload) => {
      if (!slot.handler) throw new Error('settings_changed handler not registered');
      slot.handler(payload);
    },
  };
};

// Capture the `nagori://capture_skipped` handler so a test can fire a
// dropped-capture notice into App's palette-window listener.
type CaptureSkippedPayload = { kind?: string; reasons?: string[] };
const captureCaptureSkippedHandler = (): { fire: (payload: CaptureSkippedPayload) => void } => {
  const slot: { handler?: (payload: CaptureSkippedPayload) => void } = {};
  vi.mocked(subscribe).mockImplementation((event, handler, onReady) => {
    if (event === 'nagori://capture_skipped') {
      slot.handler = handler as (payload: CaptureSkippedPayload) => void;
    }
    onReady?.();
    return () => {};
  });
  return {
    fire: (payload) => {
      if (!slot.handler) throw new Error('capture_skipped handler not registered');
      slot.handler(payload);
    },
  };
};

// Minimal settings snapshot — cast through `unknown` like the other tests so a
// case can override just the fields it exercises without spelling out every
// AppSettings key.
const settingsSnapshot = (overrides: Partial<AppSettings>): AppSettings =>
  overrides as unknown as AppSettings;

const grantedPermission: PermissionStatus = { kind: 'accessibility', state: 'granted' };

describe('App shell', () => {
  it('mounts the palette route by default', () => {
    const { container } = render(App);
    expect(container.querySelector('.app-shell')).toBeTruthy();
    expect(viewState.current).toBe('palette');
  });

  it('returns to the palette when Escape fires while on the settings route', async () => {
    showSettings();
    render(App);
    await fireEvent.keyDown(window, { key: 'Escape' });
    expect(viewState.current).toBe('palette');
  });

  it('invokes hidePalette on Escape when on the palette route inside Tauri', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    await fireEvent.keyDown(window, { key: 'Escape' });
    expect(hidePalette).toHaveBeenCalled();
  });

  it('skips the Escape handler when defaultPrevented is true', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    const event = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true });
    event.preventDefault();
    window.dispatchEvent(event);
    expect(hidePalette).not.toHaveBeenCalled();
  });

  it('hides the palette on window blur', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    await fireEvent.blur(window);
    expect(hidePalette).toHaveBeenCalled();
  });

  it('does not hide on blur when the user is on the settings route', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    showSettings();
    render(App);
    await fireEvent.blur(window);
    expect(hidePalette).not.toHaveBeenCalled();
  });

  it('closes an open context menu on Escape and leaves the palette up', async () => {
    // The menu normally swallows Escape itself once focused; this covers the
    // pre-focus frame where Escape reaches the window handler, which must close
    // only the menu rather than hide the whole palette.
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    openEntryContextMenu({
      x: 10,
      y: 10,
      targetIds: ['r1'],
      primaryPinned: false,
      offersFormatChoice: false,
    });
    await fireEvent.keyDown(window, { key: 'Escape' });
    expect(entryContextMenuState.open).toBe(false);
    expect(hidePalette).not.toHaveBeenCalled();
    closeEntryContextMenu();
  });

  it('closes an open context menu on blur (alongside hiding the palette)', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    openEntryContextMenu({
      x: 10,
      y: 10,
      targetIds: ['r1'],
      primaryPinned: false,
      offersFormatChoice: false,
    });
    await fireEvent.blur(window);
    expect(entryContextMenuState.open).toBe(false);
    expect(hidePalette).toHaveBeenCalled();
    closeEntryContextMenu();
  });

  it('detaches its window listeners on unmount', async () => {
    const { unmount } = render(App);
    unmount();
    // Once unmounted, blur should no longer reach the (now removed) handler.
    vi.mocked(isTauri).mockReturnValue(true);
    await fireEvent.blur(window);
    expect(hidePalette).not.toHaveBeenCalled();
  });

  it('re-fetches permissions when the palette window regains focus', async () => {
    // A grant made in the separate Settings webview never reaches this
    // window's store, so the palette re-fetches on focus — the moment the
    // user returns after using the Setup tab.
    vi.mocked(isTauri).mockReturnValue(true);
    render(App);
    vi.mocked(getPermissions).mockClear();
    await fireEvent.focus(window);
    await waitFor(() => {
      expect(getPermissions).toHaveBeenCalled();
    });
  });

  it('renders the hotkey-failure toast when a live emit fires after mount', async () => {
    const { fire } = captureHotkeyFailureHandler();
    const { findByText } = render(App);
    fire({ hotkey: 'Cmd+Shift+V', error: 'shortcut already registered' });
    await findByText('shortcut already registered');
  });

  it('hydrates the hotkey-failure toast from the backend cache when no live emit follows', async () => {
    // Simulate the startup race: the live `nagori://hotkey_register_failed`
    // emit fired before App's subscription was attached, so the only path
    // to the user is the backend's cached snapshot read via
    // `last_hotkey_failure` on mount.
    vi.mocked(isTauri).mockReturnValue(true);
    vi.mocked(lastHotkeyFailure).mockResolvedValue({
      hotkey: 'Cmd+Shift+V',
      error: 'startup-race captured',
    });
    const { findByText } = render(App);
    await findByText('startup-race captured');
    expect(lastHotkeyFailure).toHaveBeenCalled();
  });

  it('dismissing the hotkey toast clears the shared store', async () => {
    const { fire } = captureHotkeyFailureHandler();
    const { findByText, getByText } = render(App);
    fire({ hotkey: 'Cmd+Shift+V', error: 'shortcut already registered' });
    await findByText('shortcut already registered');
    const dismissButton = getByText('Dismiss');
    await fireEvent.click(dismissButton);
    await waitFor(() => {
      expect(hotkeyFailureState.failure).toBeUndefined();
    });
  });

  it('clears the hotkey toast when the backend emits a matching resolved event', async () => {
    // A live failure puts the banner up. Once the backend rebinds the
    // hotkey on a later reconcile, it emits the resolved event. Without
    // this path, the user is stuck with a stale toast until they
    // manually dismiss.
    const { fire, fireResolved } = captureHotkeyFailureHandler();
    const { findByText } = render(App);
    fire({ hotkey: 'Cmd+Shift+V', error: 'shortcut already registered' });
    await findByText('shortcut already registered');
    fireResolved({});
    await waitFor(() => {
      expect(hotkeyFailureState.failure).toBeUndefined();
    });
  });

  it('keeps the hotkey toast when the resolved event targets a different kind', async () => {
    // The store only displays one failure at a time, but the kind
    // discriminator on the resolved event keeps a primary success from
    // silently wiping a still-failing secondary (and vice versa).
    const { fire, fireResolved } = captureHotkeyFailureHandler();
    render(App);
    fire({
      hotkey: 'Cmd+Shift+R',
      error: 'secondary clash',
      kind: 'secondary',
      action: 'repaste-last',
    });
    await waitFor(() => {
      expect(hotkeyFailureState.failure?.error).toBe('secondary clash');
    });
    fireResolved({}); // primary kind — does not match.
    expect(hotkeyFailureState.failure?.error).toBe('secondary clash');
  });

  it('keeps the hotkey toast when a sibling secondary action resolves', async () => {
    // Two secondaries can fail at the same time; the displayed banner
    // belongs to one specific action. A resolved event scoped to a
    // *different* secondary action must not silently wipe it — the
    // backend cache keys per action, and the live store mirrors that
    // discrimination so the user keeps seeing the still-failing one.
    const { fire, fireResolved } = captureHotkeyFailureHandler();
    render(App);
    fire({
      hotkey: 'Cmd+Shift+R',
      error: 'repaste clash',
      kind: 'secondary',
      action: 'repaste-last',
    });
    await waitFor(() => {
      expect(hotkeyFailureState.failure?.error).toBe('repaste clash');
    });
    fireResolved({ kind: 'secondary', action: 'clear-history' });
    expect(hotkeyFailureState.failure?.error).toBe('repaste clash');
    fireResolved({ kind: 'secondary', action: 'repaste-last' });
    await waitFor(() => {
      expect(hotkeyFailureState.failure).toBeUndefined();
    });
  });

  it('surfaces the next cached failure after a matching resolve clears the displayed one', async () => {
    // Two secondaries fail simultaneously. The displayed banner belongs
    // to whichever the backend's cache returned first; when that one
    // resolves, the per-action cache still holds the other. The store
    // must re-hydrate so the user sees the still-failing sibling
    // instead of an empty toast slot that hides a real issue.
    vi.mocked(isTauri).mockReturnValue(true);
    // First call (initial hydration) returns null — nothing yet — so
    // the live emit fires the banner. After the resolve, the next
    // call returns the still-cached sibling failure.
    vi.mocked(lastHotkeyFailure).mockResolvedValueOnce(null).mockResolvedValueOnce({
      hotkey: 'Cmd+Shift+K',
      error: 'clear-history still clashing',
      kind: 'secondary',
      action: 'clear-history',
    });
    const { fire, fireResolved } = captureHotkeyFailureHandler();
    const { findByText } = render(App);
    fire({
      hotkey: 'Cmd+Shift+R',
      error: 'repaste clash',
      kind: 'secondary',
      action: 'repaste-last',
    });
    await findByText('repaste clash');
    fireResolved({ kind: 'secondary', action: 'repaste-last' });
    await findByText('clear-history still clashing');
    expect(hotkeyFailureState.failure?.action).toBe('clear-history');
  });

  it('re-queries the cache when a resolve interleaves with the initial hydration', async () => {
    // A backend resolve event can land while the initial
    // `lastHotkeyFailure()` query is still in flight — before any
    // banner has been displayed. The snapshot the pending query
    // captured predates the resolve, so applying it would surface an
    // entry the backend just cleared (or block a still-failing
    // sibling sitting in the cache from ever appearing). The watcher
    // bumps an epoch on every live event so the stale snapshot is
    // discarded, and re-queries on resolve so the post-resolve cache
    // state (the sibling that is still failing) gets a chance to
    // surface.
    vi.mocked(isTauri).mockReturnValue(true);
    const initialSlot: {
      resolve?: (
        value: {
          hotkey: string;
          error: string;
          kind?: string;
          action?: string;
        } | null,
      ) => void;
    } = {};
    vi.mocked(lastHotkeyFailure)
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            initialSlot.resolve = resolve;
          }),
      )
      .mockResolvedValueOnce({
        hotkey: 'Cmd+Shift+K',
        error: 'sibling still failing',
        kind: 'secondary',
        action: 'clear-history',
      });
    const { fireResolved } = captureHotkeyFailureHandler();
    const { findByText } = render(App);
    // The initial hydration query has fired and is pending; now the
    // resolve event arrives for an unrelated secondary action.
    fireResolved({ kind: 'secondary', action: 'repaste-last' });
    // Resolve the pending initial query with the now-stale entry for
    // the action that was just resolved.
    initialSlot.resolve?.({
      hotkey: 'Cmd+Shift+R',
      error: 'stale repaste failure',
      kind: 'secondary',
      action: 'repaste-last',
    });
    await findByText('sibling still failing');
    expect(hotkeyFailureState.failure?.action).toBe('clear-history');
  });

  it('does not let a stale cache hydration overwrite a fresh live failure', async () => {
    // Hydration races the live subscription: if `lastHotkeyFailure`
    // resolves *after* a live emit, applying the cached value would
    // clobber the newer failure. The watcher tracks a `liveEventSeen`
    // flag to guard against this.
    vi.mocked(isTauri).mockReturnValue(true);
    const hydrationSlot: {
      resolve?: (value: { hotkey: string; error: string } | null) => void;
    } = {};
    vi.mocked(lastHotkeyFailure).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          hydrationSlot.resolve = resolve;
        }),
    );
    const { fire } = captureHotkeyFailureHandler();
    const { findByText } = render(App);
    fire({ hotkey: 'Cmd+Shift+V', error: 'fresh live failure' });
    await findByText('fresh live failure');
    hydrationSlot.resolve?.({ hotkey: 'Cmd+Shift+V', error: 'stale cached failure' });
    // Give the microtask queue a chance to apply the resolved value
    // before asserting that it was rejected.
    await Promise.resolve();
    await Promise.resolve();
    expect(hotkeyFailureState.failure?.error).toBe('fresh live failure');
  });

  it('defers the cache query until both subscriptions report attached', async () => {
    // The live subscription's `listen()` resolves asynchronously after
    // `subscribe()` returns. If `lastHotkeyFailure` fires eagerly — as
    // it did before — a backend emit landing in the gap between
    // `subscribe()` returning and `listen()` actually attaching would
    // both miss the live listener *and* land in the cache too late for
    // the eager query to see it. Gate the query behind both
    // subscriptions' `onReady` callbacks.
    vi.mocked(isTauri).mockReturnValue(true);
    const readySlots: Array<() => void> = [];
    vi.mocked(subscribe).mockImplementation((_event, _handler, onReady) => {
      if (onReady) readySlots.push(onReady);
      return () => {};
    });
    vi.mocked(lastHotkeyFailure).mockResolvedValue({
      hotkey: 'Cmd+Shift+V',
      error: 'cached late',
    });
    render(App);
    await Promise.resolve();
    expect(lastHotkeyFailure).not.toHaveBeenCalled();
    readySlots[0]?.();
    await Promise.resolve();
    expect(lastHotkeyFailure).not.toHaveBeenCalled();
    readySlots[1]?.();
    await Promise.resolve();
    expect(lastHotkeyFailure).toHaveBeenCalled();
  });
});

describe('App auto-paste toast rules', () => {
  it('suppresses the accessibility-missing toast when Accessibility is not granted', async () => {
    // No permission seeded → resolver reads `NotRequested`. The
    // `accessibilityMissing` reason is exactly what the StatusBar indicator
    // already covers, so the toast must stay quiet — but the failure is still
    // recorded so the StatusBar can fold it into its accessibility chip.
    const { fire } = capturePasteFailedHandler();
    const { queryByText } = render(App);
    fire({ error: 'paste rejected', reason: 'accessibilityMissing' });
    await Promise.resolve();
    await Promise.resolve();
    expect(queryByText('Auto-paste failed')).toBeNull();
    expect(queryByText('paste rejected')).toBeNull();
    expect(pasteDiagnosticsState.failure?.reason).toBe('accessibilityMissing');
  });

  it('still toasts a non-accessibility failure even when Accessibility is not granted', async () => {
    // A missing `wtype` (or any non-accessibility reason) is a genuine error
    // the accessibility chip does NOT explain, so the toast fires regardless
    // of grant state and the tool rides through to the recorded diagnostic.
    const { fire } = capturePasteFailedHandler();
    const { findByText } = render(App);
    fire({ error: 'wtype is not installed', reason: 'toolMissing', tool: 'wtype' });
    await findByText('wtype is not installed');
    expect(pasteDiagnosticsState.failure?.tool).toBe('wtype');
  });

  it('shows the auto-paste toast when the grant is in place (unexpected failure)', async () => {
    settingsState.permissions = [grantedPermission];
    const { fire } = capturePasteFailedHandler();
    const { findByText } = render(App);
    fire({ error: 'target app refused the paste' });
    await findByText('target app refused the paste');
  });

  it('flashes a confirmation toast when Accessibility transitions to granted', async () => {
    // Hydrated with a not-granted snapshot: render seeds `NotRequested`, so
    // flipping to granted is the success transition that earns the ✓ toast.
    settingsState.loaded = true;
    const { findByText } = render(App);
    settingsState.permissions = [grantedPermission];
    await findByText('Accessibility granted');
  });

  it('does not flash the confirmation toast when already granted at mount', async () => {
    settingsState.permissions = [grantedPermission];
    settingsState.loaded = true;
    const { queryByText } = render(App);
    await Promise.resolve();
    await Promise.resolve();
    expect(queryByText('Accessibility granted')).toBeNull();
  });

  it('does not flash the confirmation toast on cold start when the first fetch lands granted', async () => {
    // Cold start: the store is still empty (`loaded = false`) so the resolver
    // reads `NotRequested` pre-hydration. The effect must not seed from that
    // value — otherwise the first *real* fetch returning granted looks like a
    // NotRequested→Granted transition and fires a spurious ✓ toast.
    const { queryByText } = render(App);
    // Hydration completes: `refreshSettings` flips permissions + `loaded`
    // together in one synchronous block, so the first seeded state is Granted.
    settingsState.permissions = [grantedPermission];
    settingsState.loaded = true;
    await Promise.resolve();
    await Promise.resolve();
    expect(queryByText('Accessibility granted')).toBeNull();
  });

  it('does not flash the confirmation toast when only the first permission probe failed', async () => {
    // `refreshSettings` flips `loaded` true even when only the permission leg
    // failed (the settings leg succeeded). The effect must not seed from that
    // empty-permission `NotRequested`; otherwise a later successful granted
    // probe looks like a NotRequested→Granted transition and re-flashes the ✓.
    settingsState.loaded = true;
    settingsState.permissionsErrorMessage = 'permission probe unavailable';
    const { queryByText } = render(App);
    await Promise.resolve();
    // The permission probe recovers and now reports granted.
    settingsState.permissions = [grantedPermission];
    settingsState.permissionsErrorMessage = undefined;
    await Promise.resolve();
    await Promise.resolve();
    expect(queryByText('Accessibility granted')).toBeNull();
  });

  it('shows the auto-paste toast on a genuine paste failure after a passive revoke', async () => {
    // `RevokedAfterGranted` is deliberately *not* a suppressed state: the
    // revoke itself is detected silently, but the next real paste attempt
    // that fails should surface a toast tied to the user's intent (S4 step 5).
    settingsState.permissions = [{ kind: 'accessibility', state: 'denied' }];
    settingsState.settings = {
      onboarding: {
        accessibilityPromptedAt: '2026-05-01T00:00:00Z',
        accessibilityFirstGrantedAt: '2026-05-01T00:00:00Z',
        completedAt: null,
      },
    } as unknown as NonNullable<typeof settingsState.settings>;
    settingsState.loaded = true;
    const { fire } = capturePasteFailedHandler();
    const { findByText } = render(App);
    fire({ error: 'paste rejected after revoke' });
    await findByText('paste rejected after revoke');
  });
});

describe('App capture_skipped propagation', () => {
  it('records a dropped-capture notice into the store', async () => {
    const { fire } = captureCaptureSkippedHandler();
    render(App);
    fire({ kind: 'secret_redacted_dropped', reasons: ['one_time_password_pattern'] });
    await waitFor(() => {
      expect(captureSkippedState.notice).toEqual({
        kind: 'secret_redacted_dropped',
        reasons: ['one_time_password_pattern'],
      });
    });
  });

  it('records an unknown kind from a newer daemon as a generic notice', async () => {
    const { fire } = captureCaptureSkippedHandler();
    render(App);
    fire({ kind: 'something_new', reasons: [] });
    await waitFor(() => {
      expect(captureSkippedState.notice).toEqual({ kind: 'something_new', reasons: [] });
    });
  });
});

describe('App settings_changed propagation', () => {
  it('adopts the snapshot so palette-config reads update live', async () => {
    // Settings runs in its own webview; the palette only learns of a change
    // through the broadcast event. Adopting the payload keeps the
    // `settingsState`-driven surfaces (row count, preview pane, palette
    // hotkeys, paste-format default) live instead of stale until relaunch.
    const { fire } = captureSettingsChangedHandler();
    render(App);
    fire(settingsSnapshot({ recentOrder: 'by_recency', paletteRowCount: 12 }));
    await waitFor(() => {
      expect(settingsState.settings?.paletteRowCount).toBe(12);
    });
  });

  it('re-runs the current query when recentOrder changes', async () => {
    // The sort order is applied backend-side as a search runs, so the visible
    // list keeps its old order until the palette re-issues the query.
    settingsState.settings = settingsSnapshot({ recentOrder: 'by_recency' });
    const { fire } = captureSettingsChangedHandler();
    render(App);
    fire(settingsSnapshot({ recentOrder: 'by_use_count' }));
    await waitFor(() => {
      expect(refreshCurrent).toHaveBeenCalled();
    });
  });

  it('re-runs the current query when a snapshot arrives before settings hydrate', async () => {
    // settingsState.settings is still undefined (the snapshot beat the
    // mount-time refresh). A `settings_changed` event only fires on a real
    // edit, so any results already on screen need re-sorting — the unknown
    // previous order must not skip the re-query.
    const { fire } = captureSettingsChangedHandler();
    render(App);
    fire(settingsSnapshot({ recentOrder: 'by_recency' }));
    await waitFor(() => {
      expect(refreshCurrent).toHaveBeenCalled();
    });
  });

  it('does not re-run the query when recentOrder is unchanged', async () => {
    // A snapshot that only flips an unrelated field must adopt live without
    // paying for a redundant backend search.
    settingsState.settings = settingsSnapshot({ recentOrder: 'by_recency', paletteRowCount: 8 });
    const { fire } = captureSettingsChangedHandler();
    render(App);
    fire(settingsSnapshot({ recentOrder: 'by_recency', paletteRowCount: 16 }));
    await waitFor(() => {
      expect(settingsState.settings?.paletteRowCount).toBe(16);
    });
    expect(refreshCurrent).not.toHaveBeenCalled();
  });
});

import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
  currentWindowLabel: vi.fn(() => undefined),
  subscribe: vi.fn((_event, _handler, onReady) => {
    onReady?.();
    return () => {};
  }),
  TAURI_EVENTS: {
    navigate: 'nagori://navigate',
    pasteFailed: 'nagori://paste_failed',
    hotkeyRegisterFailed: 'nagori://hotkey_register_failed',
    hotkeyRegisterResolved: 'nagori://hotkey_register_resolved',
    settingsChanged: 'nagori://settings_changed',
  },
}));

vi.mock('../lib/commands', () => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
  getCapabilities: vi.fn(),
  getPermissions: vi.fn(),
  checkForUpdates: vi.fn(),
}));

// `onMount` reaches into `@tauri-apps/api/event` to subscribe to hotkey
// failures. The runtime is unavailable in jsdom, so stub the dynamic import.
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import { getCapabilities, getPermissions, getSettings, updateSettings } from '../lib/commands';
import { isTauri, subscribe } from '../lib/tauri';
import type { AppSettings, PermissionStatus, PlatformCapabilities } from '../lib/types';
import { settingsState } from '../stores/settings.svelte';
import SettingsView from './SettingsView.svelte';

const baseSettings = (): AppSettings => ({
  globalHotkey: 'Cmd+Shift+V',
  historyRetentionCount: 1000,
  historyRetentionDays: null,
  maxEntrySizeBytes: 1024 * 1024,
  captureKinds: ['text', 'url', 'code', 'image', 'fileList', 'richText', 'unknown'],
  maxTotalBytes: null,
  captureEnabled: true,
  autoPasteEnabled: true,
  pasteFormatDefault: 'preserve',
  pasteDelayMs: 50,
  appDenylist: ['1Password'],
  regexDenylist: [],
  aiProvider: 'none',
  aiEnabled: false,
  semanticSearchEnabled: false,
  cliIpcEnabled: true,
  locale: 'en',
  recentOrder: 'by_recency',
  appearance: 'system',
  autoLaunch: false,
  secretHandling: 'store_redacted',
  paletteHotkeys: {},
  secondaryHotkeys: {},
  paletteRowCount: 8,
  showPreviewPane: true,
  showInMenuBar: true,
  clearOnQuit: false,
  captureInitialClipboardOnLaunch: true,
  autoUpdateCheck: true,
  updateChannel: 'stable',
  maxThumbnailTotalBytes: 64 * 1024 * 1024,
});

// Capability fixtures mirror what `nagori-platform-{macos,windows,linux}`
// emit from their `capabilities()` adapter. They're not aiming for an
// exhaustive enumeration — just the shape of each status variant so the
// Advanced tab's row + badge renderer is locked down per platform. If
// the backend matrix shifts (e.g. Linux gains in-process global hotkey
// support), bump the fixture and the test will catch any UI drift.
const macosCapabilities = (): PlatformCapabilities => ({
  platform: 'macos',
  tier: 'supported',
  captureText: { status: 'available' },
  captureImage: { status: 'available' },
  captureFiles: { status: 'available' },
  writeText: { status: 'available' },
  writeImage: { status: 'available' },
  clipboardMultiRepresentationWrite: { status: 'available' },
  autoPaste: {
    status: 'requiresPermission',
    permission: 'accessibility',
    message: 'Grant Accessibility access in System Settings.',
  },
  globalHotkey: { status: 'available' },
  frontmostApp: { status: 'available' },
  permissionsUi: { status: 'available' },
  updateCheck: { status: 'available' },
  previewQuickLook: { status: 'available' },
});

const windowsCapabilities = (): PlatformCapabilities => ({
  platform: 'windows',
  tier: 'supported',
  captureText: { status: 'available' },
  captureImage: { status: 'available' },
  captureFiles: { status: 'available' },
  writeText: { status: 'available' },
  writeImage: { status: 'available' },
  // Windows publishes CF_UNICODETEXT + CF_HTML + RTF + CF_DIBV5 + the
  // registered "PNG" companion + CF_HDROP in one transaction (see
  // `crates/nagori-platform-windows/src/capability.rs`), so Preserve
  // copy-back keeps every captured representation alive — multi-rep is
  // Available, not Unsupported.
  clipboardMultiRepresentationWrite: { status: 'available' },
  autoPaste: { status: 'available' },
  globalHotkey: { status: 'available' },
  frontmostApp: { status: 'available' },
  permissionsUi: {
    status: 'unsupported',
    reason:
      'Windows does not gate clipboard / input synthesis behind a user-managed permission UI; the doctor probe is a no-op.',
  },
  updateCheck: { status: 'available' },
  previewQuickLook: {
    status: 'unsupported',
    reason:
      "Windows has no OS-provided Quick-Look-equivalent overlay; the palette's preview shortcut is disabled.",
  },
});

const linuxWaylandCapabilities = (): PlatformCapabilities => ({
  platform: 'linuxWayland',
  tier: 'supported',
  captureText: { status: 'available' },
  captureImage: { status: 'available' },
  captureFiles: { status: 'available' },
  writeText: { status: 'available' },
  writeImage: { status: 'available' },
  clipboardMultiRepresentationWrite: { status: 'available' },
  autoPaste: {
    status: 'requiresExternalTool',
    tool: 'wtype',
    installHint: 'apt install wtype',
  },
  globalHotkey: {
    status: 'unsupported',
    reason: 'tauri-plugin-global-shortcut is X11-only; pure Wayland sessions fail to register.',
  },
  frontmostApp: {
    status: 'unsupported',
    reason: 'Wayland refuses to expose a portable foreground-surface query.',
  },
  permissionsUi: {
    status: 'unsupported',
    reason:
      'Wayland sessions do not gate clipboard / input synthesis behind a user-managed permission UI; the doctor probe is a no-op.',
  },
  updateCheck: { status: 'available' },
  previewQuickLook: {
    status: 'unsupported',
    reason:
      "Linux Wayland has no DE-agnostic Quick-Look-equivalent overlay; the palette's preview shortcut is disabled.",
  },
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  vi.mocked(getSettings).mockResolvedValue(baseSettings());
  // Default permissions response: empty array means `accessibilityGranted()`
  // returns false, preserving the "Needs permission" status on the
  // Auto-paste capability row for tests that don't opt in to granted state.
  vi.mocked(getPermissions).mockResolvedValue([]);
  // `settingsState` is a module-level Svelte store, so its `permissions`
  // array survives across tests. Reset it here so a previous granted-state
  // fixture cannot leak into a subsequent test that mounts before its own
  // `refreshSettings()` round-trip resolves.
  settingsState.permissions = [];
  // Default capabilities response so the existing test suite — which
  // already exercises the Advanced tab — has a deterministic stub. The
  // platform-specific tests below override this per-case.
  vi.mocked(getCapabilities).mockResolvedValue(macosCapabilities());
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

// Route the `subscribe` mock so a test can fire a `nagori://settings_changed`
// event into the SettingsView listener. The Settings view now only
// subscribes to `settings_changed`; hotkey-failure subscription lives at
// the App level (see `App.test.ts`), driven through the shared
// `hotkeyFailureState` store this view derives `hotkeyError` from.
const captureSettingsChangedHandler = (): {
  fire: (snapshot: AppSettings) => void;
} => {
  const slot: { handler?: (payload: AppSettings) => void } = {};
  vi.mocked(subscribe).mockImplementation((event, handler) => {
    if (event === 'nagori://settings_changed') {
      slot.handler = handler as (payload: AppSettings) => void;
    }
    return () => {};
  });
  return {
    fire: (snapshot) => {
      if (!slot.handler) throw new Error('settings_changed handler not registered');
      slot.handler(snapshot);
    },
  };
};

const openAdvancedTab = async (capabilities: PlatformCapabilities) => {
  vi.mocked(getCapabilities).mockResolvedValue(capabilities);
  const view = render(SettingsView);
  const advanced = await view.findByRole('tab', { name: 'Advanced' });
  await fireEvent.click(advanced);
  // Wait for the capability table to mount — `getCapabilities` is
  // resolved off the main render path, so the fieldset only appears
  // once the promise settles.
  await view.findByText('Platform capabilities');
  return view;
};

describe('SettingsView', () => {
  it('loads settings on mount and hydrates the form fields', async () => {
    const { findByRole, container } = render(SettingsView);

    // Wait for the form to render — proxied for "hydration complete" by
    // the appearance of the Back-to-palette button.
    await findByRole('button', { name: 'Back to palette' });
    expect(getSettings).toHaveBeenCalled();
    // The hotkey input on the General tab reflects the loaded settings.
    // Rendered as a record-style button showing the OS-formatted accelerator
    // (⌘⇧V on macOS) rather than the raw `Cmd+Shift+V` wire format.
    const hotkeyButton = container.querySelector('.hotkey-input .display') as HTMLButtonElement;
    expect(hotkeyButton.textContent?.trim()).toBe('⇧⌘V');

    // Switching to Privacy reveals the denylist textarea hydrated from the
    // app-denylist payload.
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);
    await waitFor(() => {
      const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
      expect(textarea?.value).toBe('1Password');
    });
  });

  it('switches the visible fieldset when a tab is clicked', async () => {
    const { findByRole, queryByText } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);
    expect(queryByText('App denylist')).toBeTruthy();
    expect(privacyTab.getAttribute('aria-selected')).toBe('true');
  });

  it('auto-saves a checkbox change with no debounce', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(captureCheckbox.checked).toBe(true);
    await fireEvent.click(captureCheckbox);

    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.captureEnabled).toBe(false);
  });

  it('coalesces a burst of number-input edits into a single update_settings call', async () => {
    // The Advanced tab's "Max bytes per entry" / "Paste delay (ms)" inputs
    // debounce on `oninput` so rapid typing doesn't fan out into one
    // round-trip per keystroke. The exact delay is an implementation
    // detail; advancing the fake clock past the upper bound (350 ms text
    // input + slack) proves the debounce fires exactly once after the
    // burst settles.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const advanced = await findByRole('tab', { name: 'Advanced' });
    await fireEvent.click(advanced);

    const numberInputs = container.querySelectorAll('input[type="number"]');
    const maxBytes = numberInputs[0] as HTMLInputElement;
    expect(maxBytes).toBeTruthy();

    // Three rapid edits — only the final value should reach the backend.
    await fireEvent.input(maxBytes, { target: { value: '2048' } });
    await fireEvent.input(maxBytes, { target: { value: '3072' } });
    await fireEvent.input(maxBytes, { target: { value: '4096' } });

    // Before the debounce window elapses, no save has fired.
    expect(updateSettings).not.toHaveBeenCalled();

    // Advance past the textarea-class window (covers number debounce too).
    await vi.advanceTimersByTimeAsync(600);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.maxEntrySizeBytes).toBe(4096);
  });

  it('does not save while a regex denylist entry is invalid', async () => {
    // The privacy preflight runs the same `compile_user_regex` checks
    // the daemon would; surfacing per-line inline guidance is more
    // actionable than letting the daemon round-trip a single opaque
    // `invalid_input`. When the user has only edited the broken regex
    // textarea, the snapshot we'd ship is identical to what's on disk
    // (we substitute the last valid list) and the equality check in
    // `commitSave` short-circuits the round-trip.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container, queryByRole } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const textareas = container.querySelectorAll('textarea');
    expect(textareas.length).toBeGreaterThanOrEqual(2);
    const regexTextarea = textareas[1] as HTMLTextAreaElement;
    // Unbalanced parens are the cheapest way to provoke an invalid-syntax
    // error without bumping the length cap.
    await fireEvent.input(regexTextarea, { target: { value: '(' } });

    await waitFor(() => {
      const alert = queryByRole('alert');
      expect(alert).toBeTruthy();
      expect(alert?.textContent ?? '').toMatch(/Line 1/);
      expect(alert?.textContent ?? '').toMatch(/invalid regex/i);
    });

    await vi.advanceTimersByTimeAsync(600);
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it('saves other-tab edits with the last valid regex while the textarea is broken', async () => {
    // The earlier silent-skip blocked every tab's save behind a broken
    // regex line — confusing if the user toggled a checkbox on General
    // and "Saved" never showed. Now `buildSnapshotPayload` substitutes
    // the last valid regex list (here: the loaded `[]`), so unrelated
    // edits still reach disk while the user is mid-fix on Privacy.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const regexTextarea = container.querySelectorAll('textarea')[1] as HTMLTextAreaElement;
    await fireEvent.input(regexTextarea, { target: { value: '(' } });
    await vi.advanceTimersByTimeAsync(600);
    expect(updateSettings).not.toHaveBeenCalled();

    const generalTab = await findByRole('tab', { name: 'General' });
    await fireEvent.click(generalTab);
    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(captureCheckbox.checked).toBe(true);
    await fireEvent.click(captureCheckbox);

    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.captureEnabled).toBe(false);
    // The persisted regex list is the last-valid one (the loaded `[]`),
    // not the broken `["("]` currently in the textarea.
    expect(sent?.regexDenylist).toEqual([]);
  });

  it('flushes a pending debounced edit when the component unmounts', async () => {
    // The user types into a debounced field (textarea / number) then
    // navigates away (Escape -> palette) before the 350/500 ms window
    // elapses. Without the unmount flush, the in-memory edit is dropped
    // when `pendingTimer` is cleared. The webview context outlives the
    // Svelte component so a fire-and-forget `updateSettings` still
    // lands.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container, unmount } = render(SettingsView);
    const advanced = await findByRole('tab', { name: 'Advanced' });
    await fireEvent.click(advanced);

    const maxBytes = container.querySelectorAll('input[type="number"]')[0] as HTMLInputElement;
    await fireEvent.input(maxBytes, { target: { value: '8192' } });
    // Mid-debounce: nothing has reached the backend yet.
    expect(updateSettings).not.toHaveBeenCalled();

    unmount();
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.maxEntrySizeBytes).toBe(8192);
  });

  it('flushes a textarea edit on unmount even without a blur event', async () => {
    // Textarea fields (app denylist, regex denylist) commit on
    // debounce-elapsed `oninput` because each keystroke fires. Escape ->
    // palette can tear the focused control off the DOM before the debounce
    // window closes; the unmount flush is the only thing keeping the
    // partial-but-complete edit alive. Hotkey fields used to share this
    // shape, but the record-style input commits on capture so they no
    // longer rely on the unmount path.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container, unmount } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    // Switch to Privacy so the app-denylist textarea is mounted.
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);
    const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
    await fireEvent.input(textarea, { target: { value: 'NewApp' } });
    // Inside the debounce window — without the unmount flush this would
    // silently vanish.
    expect(updateSettings).not.toHaveBeenCalled();

    unmount();
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.appDenylist).toEqual(['NewApp']);
  });

  it('defers the unmount flush until any in-flight save resolves', async () => {
    // Without serialisation, the flush would fire a second
    // `update_settings` in parallel with the still-pending first call.
    // The daemon's SQLite store uses a connection pool, so two writes
    // dispatched concurrently can settle out of order — the older
    // snapshot landing last would clobber the user's most recent edit.
    let resolveFirst: (() => void) | undefined;
    const firstCall = new Promise<void>((resolve) => {
      resolveFirst = resolve;
    });
    vi.mocked(updateSettings)
      .mockImplementationOnce(() => firstCall)
      .mockImplementationOnce(async () => {});

    const { findByRole, container, unmount } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    // Two distinct checkbox edits so the snapshots differ.
    const checkboxes = container.querySelectorAll('input[type="checkbox"]');
    const capture = checkboxes[0] as HTMLInputElement;
    const autoPaste = checkboxes[1] as HTMLInputElement;

    // First edit starts the in-flight save (controlled by `firstCall`).
    await fireEvent.click(capture);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // Second edit lands while the first call is still pending.
    await fireEvent.click(autoPaste);
    expect(updateSettings).toHaveBeenCalledTimes(1);

    // Unmount — the flush is chained behind `firstCall`, so the
    // backend must still see exactly one in-flight call.
    unmount();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(updateSettings).toHaveBeenCalledTimes(1);

    // Resolving the first call lets the chained flush fire.
    resolveFirst?.();
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    const second = vi.mocked(updateSettings).mock.calls[1]?.[0];
    expect(second?.captureEnabled).toBe(false);
    expect(second?.autoPasteEnabled).toBe(false);
  });

  it('retries a failed snapshot on unmount', async () => {
    // After `updateSettings` rejects the UI surfaces "save error", but
    // there's no Save button to drive a manual retry — we removed it
    // going macOS-style silent-autosave. The unmount flush is the
    // safety net: it must compare against the *persisted* baseline (not
    // the optimistically-advanced `lastSentJson`) so a failed save gets
    // one more shot on the way out instead of being silently dropped.
    vi.mocked(updateSettings)
      .mockImplementationOnce(async () => {
        throw new Error('backend transient');
      })
      .mockResolvedValueOnce(undefined);

    const { findByRole, container, unmount } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    await fireEvent.click(captureCheckbox);
    // First call rejects; UI flips to status="error".
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // Closing Settings retries the failed snapshot.
    unmount();
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    const retried = vi.mocked(updateSettings).mock.calls[1]?.[0];
    expect(retried?.captureEnabled).toBe(false);
  });

  it('retries a failed save when a follow-up edit lands', async () => {
    // Same setup as the unmount retry, but the user keeps editing
    // instead of leaving. The follow-up edit changes the snapshot, so
    // the equality short-circuit lets the new combined payload through
    // and the previously-failed fields ride along.
    vi.mocked(updateSettings)
      .mockImplementationOnce(async () => {
        throw new Error('backend transient');
      })
      .mockResolvedValueOnce(undefined);

    const { findByRole, container } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const checkboxes = container.querySelectorAll('input[type="checkbox"]');
    const capture = checkboxes[0] as HTMLInputElement;
    const autoPaste = checkboxes[1] as HTMLInputElement;

    await fireEvent.click(capture);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // A second, distinct edit must trigger a new IPC carrying both
    // changes — the first edit was sent but never persisted.
    await fireEvent.click(autoPaste);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    const second = vi.mocked(updateSettings).mock.calls[1]?.[0];
    expect(second?.captureEnabled).toBe(false);
    expect(second?.autoPasteEnabled).toBe(false);
  });

  it('retries a failed save automatically after a cool-down', async () => {
    // The two retry paths above ride on either a follow-up edit or
    // unmount. If the user does neither — common after a transient IPC
    // blip — the error pill would stay up indefinitely and the edit
    // would be stranded. The cool-down timer is the third leg: it
    // re-submits the same snapshot from the background.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings)
      .mockImplementationOnce(async () => {
        throw new Error('backend transient');
      })
      .mockResolvedValueOnce(undefined);

    const { findByRole, container } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    await fireEvent.click(captureCheckbox);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // Push past the cool-down without any further user input. The
    // identical snapshot must be re-submitted automatically — that
    // requires rewinding `lastSentJson` in the failure branch so the
    // dedup short-circuit lets the retry through.
    await vi.advanceTimersByTimeAsync(5000);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    const retried = vi.mocked(updateSettings).mock.calls[1]?.[0];
    expect(retried?.captureEnabled).toBe(false);
  });

  it('does not fan out a third IPC when a retry collides with an inflight queued drain', async () => {
    // Race that round-6 codex caught: save A is in-flight, edit B is
    // queued behind it, A fails (arms the retry). In the broken design
    // the retry would chain a third call behind B's drain even though
    // B's snapshot already subsumes A's intent. Chaining via
    // `inflight.finally` keeps the retry off the queue; B's success
    // then clears the pending retry. The contract asserted here is
    // "exactly two IPCs leave the view".
    //
    // Historically this test also covered "no partial hotkey leaks
    // into the retry payload"; the record-style HotkeyInput now makes
    // mid-typed hotkey state structurally impossible at the source, so
    // that angle moved out of the unit-test layer.
    vi.useFakeTimers({ shouldAdvanceTime: true });

    let rejectA: ((err: Error) => void) | undefined;
    const callA = new Promise<void>((_, reject) => {
      rejectA = reject;
    });
    let resolveB: (() => void) | undefined;
    const callB = new Promise<void>((resolve) => {
      resolveB = resolve;
    });
    vi.mocked(updateSettings)
      .mockImplementationOnce(() => callA)
      .mockImplementationOnce(() => callB);
    // A residual `mockResolvedValueOnce` would survive `clearAllMocks`
    // (which only resets call history) and bleed into the next test's
    // FIFO queue, so we rely on the count assertion below to catch any
    // accidental third IPC instead of stacking another `once` stub.

    const { findByRole, container } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const checkboxes = container.querySelectorAll('input[type="checkbox"]');
    const capture = checkboxes[0] as HTMLInputElement;
    const autoPaste = checkboxes[1] as HTMLInputElement;

    // Save A starts in-flight (controlled by `callA`).
    await fireEvent.click(capture);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    // Edit B lands while A is still pending — queued behind A.
    await fireEvent.click(autoPaste);
    expect(updateSettings).toHaveBeenCalledTimes(1);

    // Reject A: catch arms the retry, finally drains B from live state.
    rejectA?.(new Error('backend transient'));
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });

    // Cool-down elapses; retry chains off B and waits for it to settle.
    await vi.advanceTimersByTimeAsync(5000);
    expect(updateSettings).toHaveBeenCalledTimes(2);

    // B resolves; its success branch clears `pendingRetryJson` (B's
    // snapshot already subsumes A's intent), so the chained
    // `fireRetry` bails. Crucially, no third IPC fires.
    resolveB?.();
    await vi.advanceTimersByTimeAsync(100);
    expect(updateSettings).toHaveBeenCalledTimes(2);
  });

  it('cancels a pending retry when the user makes a fresh edit', async () => {
    // A user edit during the retry cool-down would otherwise produce
    // two near-simultaneous IPCs: the retry firing 5 s after the
    // failure and the edit firing immediately. The edit naturally
    // re-commits the latest snapshot, so the retry is redundant —
    // `scheduleSave` clears the timer to keep the IPC count honest.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings)
      .mockImplementationOnce(async () => {
        throw new Error('backend transient');
      })
      .mockResolvedValueOnce(undefined);

    const { findByRole, container } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const checkboxes = container.querySelectorAll('input[type="checkbox"]');
    const capture = checkboxes[0] as HTMLInputElement;
    const autoPaste = checkboxes[1] as HTMLInputElement;

    await fireEvent.click(capture);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // Edit lands inside the cool-down window — should immediately
    // supersede the retry instead of letting both fire.
    await vi.advanceTimersByTimeAsync(1000);
    await fireEvent.click(autoPaste);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });

    // Push well past the original cool-down deadline; no third IPC.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(updateSettings).toHaveBeenCalledTimes(2);
  });

  it('flushes a revert-to-baseline edit when a save was still in flight', async () => {
    // Race the round-8 codex review caught: edit A->B starts the
    // in-flight save (lastSentJson advances to B); before it resolves
    // the user reverts back to A; then closes Settings. With an
    // unmount-flush comparison against the persisted baseline, the
    // snapshot (= A) equals lastPersistedJson, the flush is skipped,
    // and the queued drain is then gated off by `destroyed`. The
    // in-flight B lands last, clobbering the user's revert. Comparing
    // against lastSentJson (= B) lets the unmount ship the corrective
    // A, chained behind the in-flight save.
    let resolveFirst: (() => void) | undefined;
    const firstCall = new Promise<void>((resolve) => {
      resolveFirst = resolve;
    });
    vi.mocked(updateSettings)
      .mockImplementationOnce(() => firstCall)
      .mockImplementationOnce(async () => {});

    const { findByRole, container, unmount } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    // First edit kicks off the in-flight save.
    await fireEvent.click(captureCheckbox);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    expect(vi.mocked(updateSettings).mock.calls[0]?.[0]?.captureEnabled).toBe(false);

    // Revert: toggle back to the persisted state while the first save
    // is still pending. Snapshot now matches the persisted baseline.
    await fireEvent.click(captureCheckbox);
    expect(updateSettings).toHaveBeenCalledTimes(1);

    // Unmount: the queued drain is gated off by `destroyed`, so the
    // unmount flush must carry the corrective payload (the revert).
    unmount();
    await new Promise((resolve) => setTimeout(resolve, 0));
    // The chained flush waits for the in-flight save to settle.
    expect(updateSettings).toHaveBeenCalledTimes(1);

    resolveFirst?.();
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    // Disk ends at the user's intent: the revert, not the orphaned B.
    const second = vi.mocked(updateSettings).mock.calls[1]?.[0];
    expect(second?.captureEnabled).toBe(true);
  });

  it('retries when an in-flight save fails after unmount', async () => {
    // Edge case the round-9 codex review caught: the user makes an
    // edit and closes Settings while the save is still in flight, and
    // the in-flight save then fails after `destroyed` is set. The
    // failure branch rewinds `lastSentJson` and bails on `destroyed`
    // before arming the retry timer, so the edit would be stranded
    // — there's no follow-up edit and no surface left for a manual
    // retry. Chaining the unmount flush off `inflight.finally` and
    // checking against `lastPersistedJson` at settle time ensures one
    // final dispatch goes out.
    let rejectFirst: ((err: Error) => void) | undefined;
    const firstCall = new Promise<void>((_, reject) => {
      rejectFirst = reject;
    });
    vi.mocked(updateSettings)
      .mockImplementationOnce(() => firstCall)
      .mockResolvedValueOnce(undefined);

    const { findByRole, container, unmount } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    await fireEvent.click(captureCheckbox);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // Unmount while the save is still in flight. The flush is
    // deferred until the in-flight settles.
    unmount();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(updateSettings).toHaveBeenCalledTimes(1);

    // The in-flight save rejects post-unmount. The catch branch sees
    // `destroyed` and skips the retry arm; the chained finally is the
    // only thing keeping the edit alive.
    rejectFirst?.(new Error('backend transient'));
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    const retried = vi.mocked(updateSettings).mock.calls[1]?.[0];
    expect(retried?.captureEnabled).toBe(false);
  });

  it('skips the unmount flush when nothing has changed', async () => {
    // The flush guard is an equality check against the persisted
    // baseline. Without it, every navigation back to the palette would
    // burn an idempotent IPC even when the user only opened Settings to
    // read.
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, unmount } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    unmount();
    // Give any deferred call a chance to land before asserting.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it('resumes auto-save once the regex denylist is fixed', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const regexTextarea = container.querySelectorAll('textarea')[1] as HTMLTextAreaElement;
    await fireEvent.input(regexTextarea, { target: { value: '(' } });
    await vi.advanceTimersByTimeAsync(600);
    expect(updateSettings).not.toHaveBeenCalled();

    // Repair the pattern; the next debounce tick should commit it.
    await fireEvent.input(regexTextarea, { target: { value: 'foo' } });
    await vi.advanceTimersByTimeAsync(600);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.regexDenylist).toEqual(['foo']);
  });

  it('commits a captured combo as a single atomic save', async () => {
    // The record-style HotkeyInput resolves the "partial accelerator
    // leak" problem at the source — `settings.globalHotkey` only mutates
    // when a complete combo is captured, so a single save fires per
    // capture rather than per keystroke.
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const hotkeyButton = container.querySelector('.hotkey-input .display') as HTMLButtonElement;
    expect(hotkeyButton).toBeTruthy();
    await fireEvent.click(hotkeyButton);
    // Pure-modifier press while composing — no save yet.
    await fireEvent.keyDown(hotkeyButton, {
      key: 'Meta',
      code: 'MetaLeft',
      metaKey: true,
    });
    expect(updateSettings).not.toHaveBeenCalled();

    // Complete combo lands; capture commits and triggers exactly one save.
    await fireEvent.keyDown(hotkeyButton, {
      key: 'z',
      code: 'KeyZ',
      metaKey: true,
      shiftKey: true,
    });
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.globalHotkey).toBe('CmdOrCtrl+Shift+Z');
  });

  it('coalesces edits that arrive while a save is in flight', async () => {
    // Single in-flight + queued-flag pattern: the second edit must wait
    // for the first round-trip to land, then commit one follow-up with
    // the latest snapshot. We control the first resolve manually so we
    // can interleave a second edit before it lands.
    let resolveFirst: (() => void) | undefined;
    const firstCall = new Promise<void>((resolve) => {
      resolveFirst = resolve;
    });
    let secondCallResolved = false;
    vi.mocked(updateSettings)
      .mockImplementationOnce(() => firstCall)
      .mockImplementationOnce(async () => {
        secondCallResolved = true;
      });

    const { findByRole, container } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    // First edit kicks off the in-flight call.
    await fireEvent.click(captureCheckbox);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // Second edit lands while the first is still pending; it should be
    // coalesced into a single follow-up call once the first resolves.
    await fireEvent.click(captureCheckbox);
    expect(updateSettings).toHaveBeenCalledTimes(1);

    resolveFirst?.();
    await waitFor(() => {
      expect(secondCallResolved).toBe(true);
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
  });

  it('requires confirmation before switching to store_full and skips save on cancel', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, getByDisplayValue } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const select = getByDisplayValue('Store redacted (default)') as HTMLSelectElement;

    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    await fireEvent.change(select, { target: { value: 'store_full' } });
    expect(confirmSpy).toHaveBeenCalled();
    // Cancelled confirm reverts the dropdown to the previous value and
    // must not reach the backend — declining the confirm is the explicit
    // "do not store secrets in plaintext" signal.
    expect(select.value).toBe('store_redacted');
    expect(updateSettings).not.toHaveBeenCalled();
    confirmSpy.mockRestore();
  });

  it('reports the original textarea row when blank lines precede the bad regex', async () => {
    // Regression: `linesToList` used to trim + drop empty lines before
    // validation, so the `Line N` label counted non-blank entries instead
    // of the row the user was editing. The validator now keys off the raw
    // split index so a leading blank line still produces `Line 2`.
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container, queryByRole } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const regexTextarea = container.querySelectorAll('textarea')[1] as HTMLTextAreaElement;
    await fireEvent.input(regexTextarea, { target: { value: '\n(' } });

    await waitFor(() => {
      const alert = queryByRole('alert');
      expect(alert).toBeTruthy();
      expect(alert?.textContent ?? '').toMatch(/Line 2/);
      expect(alert?.textContent ?? '').not.toMatch(/Line 1/);
    });
  });

  it('commits the store_full switch when the user accepts the warning', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, getByDisplayValue, queryByRole } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const select = getByDisplayValue('Store redacted (default)') as HTMLSelectElement;

    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    await fireEvent.change(select, { target: { value: 'store_full' } });
    expect(confirmSpy).toHaveBeenCalled();
    await waitFor(() => {
      expect(queryByRole('alert')).toBeTruthy();
    });
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.secretHandling).toBe('store_full');
    confirmSpy.mockRestore();
  });

  it('renders the tauriRequired hint outside the runtime', () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const { getByText } = render(SettingsView);
    expect(getByText('Saving settings requires the Tauri runtime.')).toBeTruthy();
  });

  it('surfaces a load error if get_settings rejects', async () => {
    vi.mocked(getSettings).mockRejectedValue(new Error('backend offline'));
    const { findByText } = render(SettingsView);
    expect(await findByText('backend offline')).toBeTruthy();
  });

  it('renders the CLI tab fieldset and saves the IPC toggle', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const cliTab = await findByRole('tab', { name: 'CLI' });
    await fireEvent.click(cliTab);

    const cliCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(cliCheckbox).toBeTruthy();
    await fireEvent.click(cliCheckbox);

    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.cliIpcEnabled).toBe(false);
  });

  it('debounces max-bytes / paste-delay edits from the Advanced tab', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const advanced = await findByRole('tab', { name: 'Advanced' });
    await fireEvent.click(advanced);

    const numberInputs = container.querySelectorAll('input[type="number"]');
    const [maxBytes, pasteDelay] = Array.from(numberInputs);
    if (maxBytes) await fireEvent.input(maxBytes, { target: { value: '4096' } });
    if (pasteDelay) await fireEvent.input(pasteDelay, { target: { value: '120' } });

    await vi.advanceTimersByTimeAsync(600);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const last = vi.mocked(updateSettings).mock.calls.at(-1)?.[0];
    expect(last?.maxEntrySizeBytes).toBe(4096);
    expect(last?.pasteDelayMs).toBe(120);
  });

  it('updates the active locale when the language picker changes', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    await findByRole('tab', { name: 'General' });
    const select = Array.from(container.querySelectorAll('select')).find((candidate) =>
      Array.from(candidate.options).some((option) => option.value === 'ja'),
    );
    expect(select).toBeTruthy();
    if (select) {
      select.value = 'ja';
      await fireEvent.change(select, { target: { value: 'ja' } });
    }
    // After the change the back-to-palette button reflects Japanese copy.
    await waitFor(() => {
      expect(container.textContent).toMatch(/パレット|戻る/);
    });
    // Locale change is a select onchange so it commits immediately.
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
  });

  it("round-trips the 'system' locale preference and renders the OS-resolved language", async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    vi.mocked(getSettings).mockResolvedValue({ ...baseSettings(), locale: 'system' });
    // Stub navigator.languages so setLocale('system') resolves to de.
    const originalLanguages = Object.getOwnPropertyDescriptor(window.navigator, 'languages');
    Object.defineProperty(window.navigator, 'languages', {
      value: ['de-DE'],
      configurable: true,
    });

    try {
      const { findByText, container } = render(SettingsView);

      // The back-to-palette button renders the German label, proving
      // setLocale('system') routed through navigator.languages to the
      // de dictionary.
      expect(await findByText('Zurück zur Palette')).toBeTruthy();

      // The dropdown keeps the 'system' preference selected rather than
      // collapsing into the concrete resolved locale.
      const localeSelect = Array.from(container.querySelectorAll('select')).find((candidate) =>
        Array.from(candidate.options).some((option) => option.value === 'system'),
      );
      expect(localeSelect).toBeTruthy();
      expect(localeSelect?.value).toBe('system');
    } finally {
      if (originalLanguages) {
        Object.defineProperty(window.navigator, 'languages', originalLanguages);
      }
    }
  });

  // ---------------- settings_changed merge (external mutations) ----------------

  it('adopts an external captureEnabled toggle from a settings_changed event', async () => {
    // The tray's "Pause Capture" menu item bypasses SettingsView and writes
    // through `set_capture_enabled`. The backend then broadcasts the new
    // snapshot via `nagori://settings_changed`; without merging, an open
    // Settings window would silently revert the tray edit on its next
    // autosave (full-snapshot semantics). The merge keeps the local view
    // in sync.
    vi.mocked(updateSettings).mockResolvedValue();
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(captureCheckbox.checked).toBe(true);

    events.fire({ ...baseSettings(), captureEnabled: false });

    await waitFor(() => {
      const cb = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
      expect(cb.checked).toBe(false);
    });
    // The event itself must not trigger an autosave — only the user's
    // edits do. Adopting remote here would otherwise echo the value
    // straight back to the backend.
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it('preserves a user-edited textarea when an external settings_changed event arrives', async () => {
    // Scenario: the user is mid-edit on the regex-denylist textarea
    // (debounces per keystroke) when the tray flips capture from
    // another window. The merge must adopt the remote `captureEnabled`
    // change (user hasn't touched it) without clobbering the
    // in-progress textarea content.
    //
    // (The hotkey field was the original carrier for this contract, but
    // the record-style HotkeyInput now commits atomically — `settings.
    // globalHotkey` never holds intermediate state, so the merge has
    // nothing per-keystroke to protect there. The textarea path is the
    // remaining live-binding surface that exercises the same merge
    // logic.)
    vi.mocked(updateSettings).mockResolvedValue();
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const appDenylistTextarea = container.querySelectorAll('textarea')[0] as HTMLTextAreaElement;
    expect(appDenylistTextarea).toBeTruthy();
    await fireEvent.input(appDenylistTextarea, { target: { value: 'MyApp' } });
    // Inside the debounce window — no save yet.
    expect(updateSettings).not.toHaveBeenCalled();

    events.fire({ ...baseSettings(), captureEnabled: false });

    // Adopted: capture flipped to false in the UI (need to flip back to
    // General to assert, since the textarea is on Privacy).
    const generalTab = await findByRole('tab', { name: 'General' });
    await fireEvent.click(generalTab);
    await waitFor(() => {
      const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
      expect(captureCheckbox.checked).toBe(false);
    });
    // Preserved: the user's textarea edit is untouched.
    await fireEvent.click(privacyTab);
    const preserved = container.querySelectorAll('textarea')[0] as HTMLTextAreaElement;
    expect(preserved.value).toBe('MyApp');
    // And no autosave fires from the merge itself.
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it('treats an echo of our own write as confirmation, not as an external change', async () => {
    // After a successful save the backend re-emits the full snapshot.
    // The echo arrives with `remoteJson === lastSentJson`; the handler
    // must not adopt or re-render — adopting a field would clobber any
    // local edits the user has started since the IPC went out.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    await fireEvent.click(captureCheckbox);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // Mid-flight, the user starts editing the app-denylist textarea. The
    // echo for the just-sent payload arrives afterwards. If the merge
    // ran for the echo and adopted `appDenylist` from the snapshot, it
    // would overwrite the user's in-progress edit — verify the echo
    // path leaves the textarea alone.
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);
    const appDenylistTextarea = container.querySelectorAll('textarea')[0] as HTMLTextAreaElement;
    expect(appDenylistTextarea).toBeTruthy();
    await fireEvent.input(appDenylistTextarea, { target: { value: 'EchoTest' } });

    // Fire the echo (matches what we sent — captureEnabled false, original denylist).
    events.fire({ ...baseSettings(), captureEnabled: false });

    expect(appDenylistTextarea.value).toBe('EchoTest');
  });

  it('preserves a mid-typed denylist textarea when an external settings_changed event arrives', async () => {
    // Denylist fields live in textarea state (`appDenylistText`) and only
    // roll into `settings.appDenylist` at save time. A naive merge that
    // compares `settings.appDenylist` against the baseline would classify
    // the field as clean while the user is mid-typing and silently
    // overwrite the textarea on the next remote event. The merge must
    // consult the textarea-derived value for the dirty check.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const appDenylistTextarea = container.querySelectorAll('textarea')[0] as HTMLTextAreaElement;
    expect(appDenylistTextarea).toBeTruthy();
    // The user types a new entry — debounce is pending, no save has fired.
    await fireEvent.input(appDenylistTextarea, { target: { value: 'KeePassXC' } });
    expect(updateSettings).not.toHaveBeenCalled();

    // External event arrives with a different appDenylist (and a different
    // captureEnabled). The merge should adopt the unrelated captureEnabled
    // change but preserve the user's in-progress textarea content.
    events.fire({
      ...baseSettings(),
      captureEnabled: false,
      appDenylist: ['Bitwarden'],
    });

    // Textarea is untouched (user-edited).
    expect(appDenylistTextarea.value).toBe('KeePassXC');

    // Let the textarea debounce fire — the snapshot dispatched should
    // carry the user's typed value, not the remote's `appDenylist`.
    await vi.advanceTimersByTimeAsync(600);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.appDenylist).toEqual(['KeePassXC']);
    // And the adopted captureEnabled went out on the same snapshot.
    expect(sent?.captureEnabled).toBe(false);
  });

  it('dispatches the debounced edit after a remote merge advances the dedup baseline', async () => {
    // Regression: `applyRemoteSettings` used to realign `lastSentJson` to
    // `buildSnapshotPayload()`, which folds in the user's debounce-pending
    // edits. When the debounce timer fired, the dedup check at the top of
    // `commitSave` would short-circuit because the snapshot it built now
    // equalled the (incorrectly advanced) `lastSentJson`, silently
    // dropping the edit. The fix realigns to the *remote* snapshot so a
    // preserved-dirty field still diverges from the dedup baseline.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    const advanced = await findByRole('tab', { name: 'Advanced' });
    await fireEvent.click(advanced);

    // Edit the first number input (debounce ~350 ms). Stay short of the
    // debounce window so the save has not fired yet.
    const maxBytes = container.querySelector('input[type="number"]') as HTMLInputElement;
    expect(maxBytes).toBeTruthy();
    await fireEvent.input(maxBytes, { target: { value: '4096' } });
    expect(updateSettings).not.toHaveBeenCalled();

    // External event mid-debounce. The merge preserves the dirty number
    // field and adopts the unrelated `captureEnabled` flip.
    events.fire({ ...baseSettings(), captureEnabled: false });

    // Now let the debounce window elapse — the save must fire with the
    // user's typed number, even though the merge moved the dedup baseline.
    await vi.advanceTimersByTimeAsync(600);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.maxEntrySizeBytes).toBe(4096);
    expect(sent?.captureEnabled).toBe(false);
  });

  it('fires a follow-up commit when an external merge lands during an in-flight save', async () => {
    // Before this fix, the success branch of `commitSave` unconditionally
    // restored `lastPersistedJson` to the in-flight snapshot, clobbering
    // the merge baseline that `applyRemoteSettings` had just advanced to
    // the remote value. The follow-up event would then be classified as
    // an echo and silently dropped, and the user's local view would
    // diverge from the backend with no IPC to reconcile.
    let firstResolve!: () => void;
    let firstCallCaptured: AppSettings | undefined;
    let secondCallCaptured: AppSettings | undefined;
    let callIndex = 0;
    vi.mocked(updateSettings).mockImplementation((s: AppSettings) => {
      const idx = callIndex;
      callIndex += 1;
      if (idx === 0) {
        firstCallCaptured = s;
        return new Promise<void>((resolve) => {
          firstResolve = resolve;
        });
      }
      secondCallCaptured = s;
      return Promise.resolve();
    });
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    // Trigger the first save (autoPasteEnabled is the second checkbox).
    const checkboxes = Array.from(
      container.querySelectorAll('input[type="checkbox"]'),
    ) as HTMLInputElement[];
    const autoPaste = checkboxes[1];
    if (!autoPaste) throw new Error('expected at least two checkboxes');
    await fireEvent.click(autoPaste);

    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    expect(firstCallCaptured?.autoPasteEnabled).toBe(false);

    // External event arrives while the first save is still in flight.
    events.fire({ ...baseSettings(), captureEnabled: false });

    // Resolve the in-flight save; the finally hook must dispatch a
    // follow-up that carries both the original local edit and the
    // adopted remote field.
    firstResolve();
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    expect(secondCallCaptured?.autoPasteEnabled).toBe(false);
    expect(secondCallCaptured?.captureEnabled).toBe(false);
  });

  it('dispatches a follow-up commit when an in-flight save fails after an external merge', async () => {
    // Failure-path counterpart to the success follow-up: when the merged
    // live snapshot happens to equal the failed dispatch (e.g. a no-op
    // external event arrives during the in-flight window, so all the
    // user's preserved-dirty fields are unchanged), leaving `lastSentJson`
    // at the failed payload would short-circuit the follow-up commit's
    // dedup check and silently drop the user's edit. The catch realigns
    // `lastSentJson` to `lastPersistedJson` (the merged remote baseline)
    // unconditionally so the follow-up still dispatches.
    let firstReject!: (e: Error) => void;
    let callIndex = 0;
    let secondCallCaptured: AppSettings | undefined;
    vi.mocked(updateSettings).mockImplementation((s: AppSettings) => {
      const idx = callIndex;
      callIndex += 1;
      if (idx === 0) {
        return new Promise<void>((_resolve, reject) => {
          firstReject = reject;
        });
      }
      secondCallCaptured = s;
      return Promise.resolve();
    });
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    const checkboxes = Array.from(
      container.querySelectorAll('input[type="checkbox"]'),
    ) as HTMLInputElement[];
    const autoPaste = checkboxes[1];
    if (!autoPaste) throw new Error('expected at least two checkboxes');
    await fireEvent.click(autoPaste);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // No-op external merge while the first save is in flight: the user's
    // preserved-dirty field stays at its edited value and every other
    // field already matched the baseline, so the post-merge live
    // snapshot equals the dispatched (and about-to-fail) payload L.
    events.fire(baseSettings());

    firstReject(new Error('boom'));
    // Without the fix, `lastSentJson` would still be L; the follow-up
    // commit would build a snapshot equal to L and dedup. With the fix,
    // `lastSentJson` is realigned to the merged baseline and the
    // follow-up dispatches.
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    expect(secondCallCaptured?.autoPasteEnabled).toBe(false);
  });

  it('still classifies a later external update correctly after the success follow-up dedups', async () => {
    // When an in-flight save succeeds but the post-merge follow-up
    // dedups (live snapshot equalled the dispatched payload, e.g. after
    // a no-op external merge), `lastPersistedJson` is intentionally left
    // at the merged remote baseline R rather than advanced to the
    // succeeded payload L. The merge algorithm is robust to this
    // divergence: user-edited fields stay dirty against R (preserved),
    // and clean fields stay clean against R (adopted from any later
    // external T). Lock that in so a future "advance to L for safety"
    // refactor cannot silently regress the preserved-dirty contract.
    let firstResolve!: () => void;
    let callIndex = 0;
    vi.mocked(updateSettings).mockImplementation(() => {
      const idx = callIndex;
      callIndex += 1;
      if (idx === 0) {
        return new Promise<void>((resolve) => {
          firstResolve = resolve;
        });
      }
      return Promise.resolve();
    });
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    const checkboxes = Array.from(
      container.querySelectorAll('input[type="checkbox"]'),
    ) as HTMLInputElement[];
    const autoPaste = checkboxes[1];
    if (!autoPaste) throw new Error('expected at least two checkboxes');
    // User flips autoPaste (dirty edit). Dispatched as L.
    await fireEvent.click(autoPaste);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // No-op external merge during the in-flight L. Local snapshot is
    // unchanged so the follow-up will dedup.
    events.fire(baseSettings());
    firstResolve();
    // Drain finally + the dedup'd follow-up.
    await Promise.resolve();
    await Promise.resolve();

    // Now a real external T flips captureEnabled. With baseline still
    // at the no-op R (=base), captureEnabled is clean (local=true=R)
    // and gets adopted; autoPaste is still dirty (local=false vs
    // R=true) and stays preserved at the user's value.
    events.fire({ ...baseSettings(), captureEnabled: false });

    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    await waitFor(() => {
      expect(captureCheckbox.checked).toBe(false);
    });
    expect(autoPaste.checked).toBe(false);
  });

  it('preserves an invalid mid-typed regex denylist when an external event arrives', async () => {
    // When the regex textarea is invalid the snapshot wire format
    // substitutes `lastValidRegexList`, so an "effective value" based
    // dirty check (the original fix's first attempt) would see local
    // equal to baseline and overwrite the user's broken-but-in-progress
    // textarea on the next remote event. The fix compares raw textarea
    // text instead, so a half-typed regex like `(` is correctly
    // classified as user-edited and preserved.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const textareas = container.querySelectorAll('textarea');
    const regexTextarea = textareas[1] as HTMLTextAreaElement;
    await fireEvent.input(regexTextarea, { target: { value: '(' } });

    // External event with a different `regexDenylist` value. The merge
    // must keep the user's invalid in-progress text intact.
    events.fire({
      ...baseSettings(),
      captureEnabled: false,
      regexDenylist: ['valid.*pattern'],
    });

    expect(regexTextarea.value).toBe('(');
    // The captureEnabled flip is adopted into local state — verify it on
    // the General tab. Privacy doesn't render the captureEnabled
    // checkbox so we can't assert on it without switching back.
    const generalTab = await findByRole('tab', { name: 'General' });
    await fireEvent.click(generalTab);
    await waitFor(() => {
      const cb = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
      expect(cb.checked).toBe(false);
    });
    // The invalid regex never reaches the backend — even if the debounce
    // window elapses, the snapshot dedup short-circuits because the
    // wire-format value (substituted `lastValidRegexList` = `[]`) matches
    // what was last sent.
    await vi.advanceTimersByTimeAsync(600);
    // captureEnabled toggle adopted from remote does not re-emit either
    // (the merge only mutates local state).
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it('schedules a fresh commit after an external merge cancels a retry timer', async () => {
    // If a previous save failed and a retry is armed (`pendingRetryJson`
    // captured the rejected snapshot), an external event must not just
    // cancel the timer — the user's preserved-dirty edit would then sit
    // unflushed until the next manual edit or unmount. The merge path
    // schedules an immediate follow-up commit so the merged live state
    // still reaches the backend.
    let callIndex = 0;
    let secondCallCaptured: AppSettings | undefined;
    vi.mocked(updateSettings).mockImplementation((s: AppSettings) => {
      const idx = callIndex;
      callIndex += 1;
      if (idx === 0) return Promise.reject(new Error('boom'));
      secondCallCaptured = s;
      return Promise.resolve();
    });
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    // First save fails — autoPasteEnabled flip dispatches and rejects.
    const checkboxes = Array.from(
      container.querySelectorAll('input[type="checkbox"]'),
    ) as HTMLInputElement[];
    const autoPaste = checkboxes[1];
    if (!autoPaste) throw new Error('expected at least two checkboxes');
    await fireEvent.click(autoPaste);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    // The catch branch arms a retry timer with the failed snapshot.

    // External event arrives before the retry fires. The merge cancels
    // the retry and schedules a fresh commit from current live state
    // (which still has the user's autoPasteEnabled flip).
    events.fire({ ...baseSettings(), captureEnabled: false });

    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    // The follow-up snapshot carries both the user's original flip and
    // the adopted remote field — not the stale failed snapshot.
    expect(secondCallCaptured?.autoPasteEnabled).toBe(false);
    expect(secondCallCaptured?.captureEnabled).toBe(false);
  });

  it('keeps the merged baseline when a pre-merge echo arrives after an in-flight external merge', async () => {
    // After [in-flight L] → [external merge R1 advances `lastPersistedJson`]
    // → [echo of L lands] the echo handler used to rewind
    // `lastPersistedJson` to the pre-merge snapshot. A subsequent
    // external R2 then evaluated against a stale baseline and could
    // mis-classify already-adopted fields as user-edited (preserving
    // them and silently dropping R2's update). The fix skips the echo
    // baseline-advance while `externalMergeDuringInflight` is set.
    let firstResolve!: () => void;
    let callIndex = 0;
    vi.mocked(updateSettings).mockImplementation(() => {
      const idx = callIndex;
      callIndex += 1;
      if (idx === 0) {
        return new Promise<void>((resolve) => {
          firstResolve = resolve;
        });
      }
      return Promise.resolve();
    });
    const events = captureSettingsChangedHandler();
    const { findByRole, container } = render(SettingsView);

    await findByRole('button', { name: 'Back to palette' });
    const checkboxes = Array.from(
      container.querySelectorAll('input[type="checkbox"]'),
    ) as HTMLInputElement[];
    const autoPaste = checkboxes[1];
    if (!autoPaste) throw new Error('expected at least two checkboxes');
    // L = post-click snapshot (autoPasteEnabled = false).
    await fireEvent.click(autoPaste);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });

    // R1 flips captureEnabled externally. Merge adopts capture, preserves
    // autoPaste (user edited). Baseline advances to R1.
    events.fire({ ...baseSettings(), captureEnabled: false });
    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    await waitFor(() => {
      expect(captureCheckbox.checked).toBe(false);
    });

    // Echo of L (matches `lastSentJson`) lands before the IPC resolves.
    // Without the fix, this would rewind `lastPersistedJson` to L.
    events.fire({ ...baseSettings(), autoPasteEnabled: false });

    // R2 arrives. It flips captureEnabled back to true and autoPasteEnabled
    // back to true. With the correct baseline (R1), `captureEnabled` is
    // local==baseline==false → clean → adopt R2's true. `autoPasteEnabled`
    // is local(false)==baseline(true)? No, baseline R1 has autoPaste=true
    // (R1 didn't change it), local has autoPaste=false (user) → dirty →
    // preserve. With a rewound baseline (L), captureEnabled would be
    // local(false) vs baseline(true) → dirty → preserved at false.
    events.fire({ ...baseSettings(), captureEnabled: true, autoPasteEnabled: true });

    // Adopted: captureEnabled flipped back to true (proves baseline was
    // R1, not the stale L).
    await waitFor(() => {
      expect(captureCheckbox.checked).toBe(true);
    });
    // Preserved: autoPaste stays at the user's edited value.
    expect(autoPaste.checked).toBe(false);

    // Drain the in-flight save so the test doesn't leak a pending
    // promise into the next case. The follow-up commit scheduled by
    // `externalMergeDuringInflight` may dedup (post-merge local state
    // can equal the pre-merge dispatch — here it does, since R2's
    // adoption of `captureEnabled` cancels the user's `autoPaste` edit
    // back to the L snapshot net of both fields) and that is correct:
    // the wire-format equality short-circuit is the whole point of
    // `lastSentJson`. The behavioural assertion above (captureCheckbox
    // adopted R2's value) already proves the baseline was R1, not the
    // stale L, which is what this test is gating against.
    firstResolve();
    await Promise.resolve();
    await Promise.resolve();
  });
});

// Each row label rendered by `capabilityRows` in SettingsView.svelte.
// Order matters for the readout assertion: rows are rendered in this
// order and the test pairs them positionally with the per-platform
// expectations below.
const CAPABILITY_LABELS = [
  'Capture text',
  'Capture image',
  'Capture files',
  'Write text',
  'Write image',
  'Multi-representation copy-back',
  'Auto-paste',
  'Global hotkey',
  'Frontmost app',
  'Permissions UI',
  'Update check',
  'Preview (Quick Look)',
] as const;

// Status badge labels emitted by `capabilityStatusLabel`. Locks the
// human-readable mapping so a refactor to the enum surface can't
// silently change what shows up in the table.
const STATUS_BADGE = {
  available: 'Available',
  unsupported: 'Unsupported',
  requiresPermission: 'Needs permission',
  requiresExternalTool: 'External tool',
  experimental: 'Experimental',
} as const;

const readCapabilityTable = (
  container: HTMLElement,
): {
  platform: string;
  tier: string;
  rows: { label: string; status: string; detail: string }[];
} => {
  const meta = container.querySelector('.capability-meta');
  const platform =
    meta?.querySelector('span:nth-of-type(1)')?.textContent?.replace('Platform:', '').trim() ?? '';
  const tier =
    meta?.querySelector('span:nth-of-type(2)')?.textContent?.replace('Tier:', '').trim() ?? '';
  const rows = Array.from(container.querySelectorAll('.capability-table tbody tr')).map((row) => ({
    label: row.querySelector('.capability-label')?.textContent?.trim() ?? '',
    status: row.querySelector('.capability-status')?.textContent?.trim() ?? '',
    detail: row.querySelector('.capability-detail')?.textContent?.trim() ?? '',
  }));
  return { platform, tier, rows };
};

describe('SettingsView Advanced tab — capability table', () => {
  it('renders macOS capabilities — every cap available except Accessibility-gated auto-paste', async () => {
    const { container } = await openAdvancedTab(macosCapabilities());
    const table = readCapabilityTable(container);

    expect(table.platform).toBe('macos');
    expect(table.tier).toBe('supported');
    expect(table.rows.map((r) => r.label)).toEqual([...CAPABILITY_LABELS]);

    // Auto-paste is the only Permission-gated cap on macOS — surfaced
    // so onboarding can prompt the user to grant Accessibility.
    const autoPaste = table.rows.find((r) => r.label === 'Auto-paste');
    expect(autoPaste?.status).toBe(STATUS_BADGE.requiresPermission);
    expect(autoPaste?.detail).toContain('accessibility');
    expect(autoPaste?.detail).toContain('Grant Accessibility access');

    // Every other cap should be `Available`.
    const others = table.rows.filter((r) => r.label !== 'Auto-paste');
    for (const row of others) {
      expect(row.status, `${row.label} should be Available on macOS`).toBe(STATUS_BADGE.available);
    }
  });

  it('flips Auto-paste to Available when Accessibility is granted', async () => {
    // The backend capability matrix is intentionally static — it only
    // reports "the OS could do it, given the Accessibility permission",
    // and is unaware of the live grant state. Merging the
    // `PermissionChecker` snapshot in the view layer is what turns the
    // row from "Needs permission" into "Available" once the user has
    // actually toggled Accessibility on in System Settings; this test
    // pins that merge so a regression cannot silently strand the row at
    // "Needs permission" while real paste flows succeed.
    const grantedAccessibility: PermissionStatus = {
      kind: 'accessibility',
      state: 'granted',
    };
    vi.mocked(getPermissions).mockResolvedValue([grantedAccessibility]);

    const { container } = await openAdvancedTab(macosCapabilities());

    // The capability table mounts as soon as `getCapabilities` resolves,
    // but `getPermissions` rides a separate promise — wait for the
    // merged badge to land instead of asserting synchronously.
    await waitFor(() => {
      const table = readCapabilityTable(container);
      const autoPaste = table.rows.find((r) => r.label === 'Auto-paste');
      expect(autoPaste?.status).toBe(STATUS_BADGE.available);
      // The "requires permission" detail string must also disappear so
      // the row reads cleanly as `Available` without trailing
      // onboarding-style hint text.
      expect(autoPaste?.detail).toBe('');
    });
  });

  it('renders Windows capabilities — permissions UI Unsupported, updates Available', async () => {
    const { container } = await openAdvancedTab(windowsCapabilities());
    const table = readCapabilityTable(container);

    expect(table.platform).toBe('windows');
    expect(table.tier).toBe('supported');

    const expectedStatus: Record<string, string> = {
      'Capture text': STATUS_BADGE.available,
      'Capture image': STATUS_BADGE.available,
      'Capture files': STATUS_BADGE.available,
      'Write text': STATUS_BADGE.available,
      'Write image': STATUS_BADGE.available,
      'Multi-representation copy-back': STATUS_BADGE.available,
      'Auto-paste': STATUS_BADGE.available,
      'Global hotkey': STATUS_BADGE.available,
      'Frontmost app': STATUS_BADGE.available,
      'Permissions UI': STATUS_BADGE.unsupported,
      'Update check': STATUS_BADGE.available,
      'Preview (Quick Look)': STATUS_BADGE.unsupported,
    };
    for (const row of table.rows) {
      expect(row.status, `unexpected badge for ${row.label}`).toBe(expectedStatus[row.label]);
    }

    // The unsupported reasons should surface as detail text — the
    // onboarding UI reads these to explain why a feature is greyed out.
    const permissions = table.rows.find((r) => r.label === 'Permissions UI');
    expect(permissions?.detail).toContain('permission UI');
  });

  it('renders Linux Wayland capabilities — wtype external tool + global hotkey unsupported', async () => {
    const { container } = await openAdvancedTab(linuxWaylandCapabilities());
    const table = readCapabilityTable(container);

    expect(table.platform).toBe('linuxWayland');
    expect(table.tier).toBe('supported');

    const expectedStatus: Record<string, string> = {
      'Capture text': STATUS_BADGE.available,
      'Capture image': STATUS_BADGE.available,
      'Capture files': STATUS_BADGE.available,
      'Write text': STATUS_BADGE.available,
      'Write image': STATUS_BADGE.available,
      'Multi-representation copy-back': STATUS_BADGE.available,
      'Auto-paste': STATUS_BADGE.requiresExternalTool,
      'Global hotkey': STATUS_BADGE.unsupported,
      'Frontmost app': STATUS_BADGE.unsupported,
      'Permissions UI': STATUS_BADGE.unsupported,
      'Update check': STATUS_BADGE.available,
      'Preview (Quick Look)': STATUS_BADGE.unsupported,
    };
    for (const row of table.rows) {
      expect(row.status, `unexpected badge for ${row.label}`).toBe(expectedStatus[row.label]);
    }

    // Auto-paste detail must surface both the tool name and the
    // install hint so the user knows what to apt-install.
    const autoPaste = table.rows.find((r) => r.label === 'Auto-paste');
    expect(autoPaste?.detail).toContain('wtype');
    expect(autoPaste?.detail).toContain('apt install wtype');

    // Global hotkey explanation covers the X11-only upstream constraint
    // that motivates the README's Linux footnote.
    const globalHotkey = table.rows.find((r) => r.label === 'Global hotkey');
    expect(globalHotkey?.detail).toContain('X11-only');
  });
});

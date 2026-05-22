import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
  subscribe: vi.fn(() => () => {}),
  TAURI_EVENTS: {
    navigate: 'nagori://navigate',
    pasteFailed: 'nagori://paste_failed',
    hotkeyRegisterFailed: 'nagori://hotkey_register_failed',
  },
}));

vi.mock('../lib/commands', () => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
  getCapabilities: vi.fn(),
  checkForUpdates: vi.fn(),
}));

// `onMount` reaches into `@tauri-apps/api/event` to subscribe to hotkey
// failures. The runtime is unavailable in jsdom, so stub the dynamic import.
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import { getCapabilities, getSettings, updateSettings } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { AppSettings, PlatformCapabilities } from '../lib/types';
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
  // Default capabilities response so the existing test suite — which
  // already exercises the Advanced tab — has a deterministic stub. The
  // platform-specific tests below override this per-case.
  vi.mocked(getCapabilities).mockResolvedValue(macosCapabilities());
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('SettingsView', () => {
  it('loads settings on mount and hydrates the form fields', async () => {
    const { findByRole, container } = render(SettingsView);

    // Wait for the form to render — proxied for "hydration complete" by
    // the appearance of the Back-to-palette button.
    await findByRole('button', { name: 'Back to palette' });
    expect(getSettings).toHaveBeenCalled();
    // The hotkey input on the General tab reflects the loaded settings.
    const hotkeyInput = container.querySelector('input[type="text"]') as HTMLInputElement;
    expect(hotkeyInput.value).toBe('Cmd+Shift+V');

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

  it('flushes a hotkey edit on unmount even without a blur event', async () => {
    // Hotkey fields commit on `onblur` because partial accelerator
    // strings ("Cmd+Sh…") would churn the OS-level shortcut. But
    // Escape -> palette tears the focused input off the DOM without
    // firing `blur`, so an unmount flush is the only thing keeping the
    // edit alive.
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container, unmount } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const hotkeyInput = container.querySelector('input[type="text"]') as HTMLInputElement;
    await fireEvent.input(hotkeyInput, { target: { value: 'Ctrl+Alt+P' } });
    // No blur fired — without the unmount flush this would silently
    // vanish.
    expect(updateSettings).not.toHaveBeenCalled();

    unmount();
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.globalHotkey).toBe('Ctrl+Alt+P');
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

  it('does not leak live state when a retry collides with an inflight queued drain', async () => {
    // Race that round-6 codex caught: save A is in-flight, edit B is
    // queued behind it, A fails (arms the retry), the queued drain
    // sends B from live state. In the broken design the retry's
    // `commitSave(overrideA)` would queue behind B, lose its override
    // on the `queued` flag, and the post-B drain would rebuild from
    // live state — leaking a mid-typed hotkey. Chaining via
    // `inflight.finally` keeps the retry off the queue; B's success
    // then clears the pending retry (its snapshot subsumes A's). The
    // contract this asserts: no IPC ever carries the partial hotkey.
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
      .mockImplementationOnce(() => callB)
      // Safety net: an accidental third IPC would land here and the
      // count assertion below would catch it.
      .mockResolvedValueOnce(undefined);

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

    // Mid-flight: the user types a partial accelerator. Live state
    // mutates per-keystroke; the bug was this leaking into a post-B
    // drain.
    const hotkey = container.querySelector('input[type="text"]') as HTMLInputElement;
    await fireEvent.input(hotkey, { target: { value: 'Cmd+Sh' } });

    // Cool-down elapses; retry chains off B and waits for it to settle.
    await vi.advanceTimersByTimeAsync(5000);
    expect(updateSettings).toHaveBeenCalledTimes(2);

    // B resolves; its success branch clears `pendingRetryJson` (B's
    // snapshot already subsumes A's intent), so the chained
    // `fireRetry` bails. Crucially, no third IPC ever fires with the
    // partial hotkey.
    resolveB?.();
    await vi.advanceTimersByTimeAsync(100);
    expect(updateSettings).toHaveBeenCalledTimes(2);

    const callsArgs = vi.mocked(updateSettings).mock.calls;
    // Both committed snapshots carry the base hotkey — the partial
    // "Cmd+Sh" must never reach the backend.
    expect(callsArgs[0]?.[0]?.globalHotkey).toBe('Cmd+Shift+V');
    expect(callsArgs[1]?.[0]?.globalHotkey).toBe('Cmd+Shift+V');
  });

  it('does not leak a mid-typed hotkey into the retry payload', async () => {
    // `bind:value={settings.globalHotkey}` updates live state on every
    // keystroke; the save trigger is `onblur` so partial accelerators
    // like "Cmd+Sh" never reach the OS-level hotkey registration. The
    // retry has to honor that contract — replaying the snapshot
    // captured at failure time, not whatever the live state holds when
    // the timer fires.
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

    // Type a partial accelerator without blurring. Live state advances
    // per-keystroke; the failed payload must not be rebuilt from it.
    const hotkeyInput = container.querySelector('input[type="text"]') as HTMLInputElement;
    await fireEvent.input(hotkeyInput, { target: { value: 'Cmd+Sh' } });

    await vi.advanceTimersByTimeAsync(5000);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(2);
    });
    const retried = vi.mocked(updateSettings).mock.calls[1]?.[0];
    expect(retried?.captureEnabled).toBe(false);
    expect(retried?.globalHotkey).toBe('Cmd+Shift+V');
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

  it('does not save the global hotkey on every keystroke, but commits on blur', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    await findByRole('button', { name: 'Back to palette' });

    const hotkeyInput = container.querySelector('input[type="text"]') as HTMLInputElement;
    await fireEvent.input(hotkeyInput, { target: { value: 'Cmd+Shift+Z' } });
    // Even past the longest debounce, oninput on a hotkey field never
    // schedules a save — the registration churn cost is too high.
    await vi.advanceTimersByTimeAsync(1000);
    expect(updateSettings).not.toHaveBeenCalled();

    await fireEvent.blur(hotkeyInput);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalledTimes(1);
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.globalHotkey).toBe('Cmd+Shift+Z');
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
  requiresPermission: 'Permission',
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

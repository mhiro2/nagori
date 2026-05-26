import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('./commands', () => ({
  requestAccessibility: vi.fn(async () => ({ kind: 'accessibility', state: 'granted' })),
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
}));

vi.mock('../stores/settings.svelte', () => ({
  refreshSettings: vi.fn(async () => undefined),
}));

import { refreshSettings } from '../stores/settings.svelte';
import {
  resetPollerForTests,
  refreshPermissionsOnce,
  resolvePermissionUiState,
  subscribeToPolling,
} from './permissions';
import { isTauri } from './tauri';
import type { OnboardingSettings, PermissionStatus, Platform } from './types';

const onboarding = (overrides: Partial<OnboardingSettings> = {}): OnboardingSettings => ({
  accessibilityPromptedAt: null,
  accessibilityFirstGrantedAt: null,
  completedAt: null,
  ...overrides,
});

const accessibility = (state: PermissionStatus['state']): PermissionStatus => ({
  kind: 'accessibility',
  state,
});

const dispatchVisibility = (state: DocumentVisibilityState): void => {
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => state,
  });
  document.dispatchEvent(new Event('visibilitychange'));
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
  vi.mocked(isTauri).mockReturnValue(true);
  // Default to a visible document — individual tests flip it to drive
  // the pause / resume branches.
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => 'visible' as DocumentVisibilityState,
  });
});

afterEach(() => {
  resetPollerForTests();
  vi.useRealTimers();
});

describe('resolvePermissionUiState', () => {
  it('returns Unavailable when the platform itself is unsupported', () => {
    expect(
      resolvePermissionUiState(undefined, onboarding(), 'unsupported' satisfies Platform),
    ).toBe('Unavailable');
  });

  it('returns NotRequested when the backend snapshot is missing', () => {
    expect(resolvePermissionUiState(undefined, onboarding(), 'macos')).toBe('NotRequested');
  });

  it('returns Unavailable when the backend marks the kind unsupported', () => {
    expect(resolvePermissionUiState(accessibility('unsupported'), onboarding(), 'macos')).toBe(
      'Unavailable',
    );
  });

  it('returns Granted when the backend reports a live grant', () => {
    expect(resolvePermissionUiState(accessibility('granted'), onboarding(), 'macos')).toBe(
      'Granted',
    );
  });

  it('returns RevokedAfterGranted when the user previously granted then turned it off', () => {
    expect(
      resolvePermissionUiState(
        accessibility('denied'),
        onboarding({
          accessibilityPromptedAt: '2024-01-01T00:00:00Z',
          accessibilityFirstGrantedAt: '2024-01-02T00:00:00Z',
        }),
        'macos',
      ),
    ).toBe('RevokedAfterGranted');
  });

  it('returns PromptShownNotGranted after the prompt has fired without a grant', () => {
    expect(
      resolvePermissionUiState(
        accessibility('notDetermined'),
        onboarding({ accessibilityPromptedAt: '2024-01-01T00:00:00Z' }),
        'macos',
      ),
    ).toBe('PromptShownNotGranted');
  });

  it('treats a backend denial without an onboarding marker as PromptShownNotGranted', () => {
    expect(resolvePermissionUiState(accessibility('denied'), onboarding(), 'macos')).toBe(
      'PromptShownNotGranted',
    );
  });

  it('returns NotRequested when nothing has happened yet', () => {
    expect(resolvePermissionUiState(accessibility('notDetermined'), onboarding(), 'macos')).toBe(
      'NotRequested',
    );
  });

  it('returns Granted on Windows where the daemon reports a synthetic grant', () => {
    // Windows does not gate global keyboard access behind a TCC-equivalent
    // prompt, so the daemon always reports `granted` — the resolver must
    // honour that before the non-macOS short-circuit kicks in.
    expect(resolvePermissionUiState(accessibility('granted'), onboarding(), 'windows')).toBe(
      'Granted',
    );
  });

  it('returns Unavailable on Windows when the backend reports anything but granted', () => {
    expect(resolvePermissionUiState(accessibility('denied'), onboarding(), 'windows')).toBe(
      'Unavailable',
    );
  });

  it('returns Unavailable on Linux Wayland regardless of the denied reason', () => {
    // The Wayland surface reports `denied` when the `wtype` helper is missing
    // — that is an install step, not a TCC retry, so we route to the same
    // Unavailable copy as Windows.
    expect(resolvePermissionUiState(accessibility('denied'), onboarding(), 'linuxWayland')).toBe(
      'Unavailable',
    );
  });

  it('returns Granted on Linux Wayland when the helper is present', () => {
    expect(resolvePermissionUiState(accessibility('granted'), onboarding(), 'linuxWayland')).toBe(
      'Granted',
    );
  });
});

describe('subscribeToPolling', () => {
  it('fires an initial fetch on first subscribe and ticks on the interval', async () => {
    subscribeToPolling();
    // Microtask drain so the immediate fetch resolves before we assert.
    await vi.advanceTimersByTimeAsync(0);
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(2000);
    expect(refreshSettings).toHaveBeenCalledTimes(2);

    await vi.advanceTimersByTimeAsync(2000);
    expect(refreshSettings).toHaveBeenCalledTimes(3);
  });

  it('shares a single interval across concurrent subscribers (refcount)', async () => {
    subscribeToPolling();
    await vi.advanceTimersByTimeAsync(0);
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    // Adding a second subscriber must not double the fetches: a peek at
    // setInterval throughput is the contract we care about.
    subscribeToPolling();
    await vi.advanceTimersByTimeAsync(0);
    // Second subscribe doesn't re-fire the initial fetch.
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(2000);
    // One tick → one extra fetch, not two.
    expect(refreshSettings).toHaveBeenCalledTimes(2);
  });

  it('keeps polling while at least one subscriber remains; stops when the last detaches', async () => {
    const offA = subscribeToPolling();
    const offB = subscribeToPolling();
    await vi.advanceTimersByTimeAsync(0);
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    offA();
    await vi.advanceTimersByTimeAsync(2000);
    expect(refreshSettings).toHaveBeenCalledTimes(2);

    offB();
    // Last subscriber detached — no more ticks.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refreshSettings).toHaveBeenCalledTimes(2);
  });

  it('pauses while the document is hidden and resumes on visibilitychange→visible', async () => {
    subscribeToPolling();
    await vi.advanceTimersByTimeAsync(0);
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    dispatchVisibility('hidden');
    await vi.advanceTimersByTimeAsync(6000);
    // Hidden: no further fetches landed.
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    dispatchVisibility('visible');
    // Coming back visible: immediate refresh.
    await vi.advanceTimersByTimeAsync(0);
    expect(refreshSettings).toHaveBeenCalledTimes(2);

    await vi.advanceTimersByTimeAsync(2000);
    expect(refreshSettings).toHaveBeenCalledTimes(3);
  });

  it('stops the interval on window blur and resumes with a one-shot fetch on focus', async () => {
    subscribeToPolling();
    await vi.advanceTimersByTimeAsync(0);
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    window.dispatchEvent(new Event('blur'));
    await vi.advanceTimersByTimeAsync(6000);
    // Blurred: no extra fetches.
    expect(refreshSettings).toHaveBeenCalledTimes(1);

    window.dispatchEvent(new Event('focus'));
    await vi.advanceTimersByTimeAsync(0);
    // Focus triggers an immediate one-shot fetch.
    expect(refreshSettings).toHaveBeenCalledTimes(2);

    await vi.advanceTimersByTimeAsync(2000);
    expect(refreshSettings).toHaveBeenCalledTimes(3);
  });

  it('emits a timeout event after 60 s and stops the interval', async () => {
    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);

    // 29 ticks at 2 s each = 58 s of normal ticks. Then the 30th tick at
    // t=60 s should land as the timeout.
    await vi.advanceTimersByTimeAsync(2000 * 29);
    expect(events.filter((e) => e === 'tick')).toHaveLength(29);
    expect(events).not.toContain('timeout');

    await vi.advanceTimersByTimeAsync(2000);
    expect(events).toContain('timeout');

    // After the timeout the interval should be torn down — no further
    // fetches even though a subscriber is still attached.
    const callsAtTimeout = vi.mocked(refreshSettings).mock.calls.length;
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refreshSettings).toHaveBeenCalledTimes(callsAtTimeout);
  });
});

describe('refreshPermissionsOnce', () => {
  it('skips IPC outside the Tauri runtime', async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    await refreshPermissionsOnce();
    expect(refreshSettings).not.toHaveBeenCalled();
  });

  it('routes through refreshSettings inside Tauri', async () => {
    await refreshPermissionsOnce();
    expect(refreshSettings).toHaveBeenCalledTimes(1);
  });
});

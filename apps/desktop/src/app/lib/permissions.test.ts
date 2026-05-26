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
  accessibilityGranted: vi.fn(() => false),
}));

import { accessibilityGranted, refreshSettings } from '../stores/settings.svelte';
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
  vi.mocked(accessibilityGranted).mockReturnValue(false);
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

  it('keeps polling instead of timing out when a grant lands in the final window', async () => {
    // The grant arrives via the fetch the *timeout tick itself* fires, so the
    // synchronous pre-fetch check still sees `denied`. The continuation must
    // re-read the fresh snapshot, suppress the timeout, and keep the interval
    // alive so a later revoke is still observable.
    let fetchCount = 0;
    vi.mocked(refreshSettings).mockImplementation(async () => {
      fetchCount += 1;
      // 1 initial fetch + 29 ticks + the timeout tick = the 31st fetch is the
      // one fired at t=60 s. Reflect a grant landing in that window.
      if (fetchCount >= 31) vi.mocked(accessibilityGranted).mockReturnValue(true);
    });
    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);

    // Advance through the 60 s budget, landing on the timeout tick.
    await vi.advanceTimersByTimeAsync(2000 * 30);
    // The grant surfaced in the final window, so no timeout is emitted...
    expect(events).not.toContain('timeout');

    // ...and the poller keeps ticking (revoke-watching mode).
    const callsSoFar = vi.mocked(refreshSettings).mock.calls.length;
    await vi.advanceTimersByTimeAsync(2000);
    expect(vi.mocked(refreshSettings).mock.calls.length).toBeGreaterThan(callsSoFar);
  });

  it('keeps polling past 60 s without a timeout once the permission is granted', async () => {
    // A granted Setup tab left open must keep polling so a later revoke is
    // caught without waiting for a focus/visibility change. The timeout only
    // caps the wait for the first grant, so once `accessibilityGranted()`
    // reports true the interval never tears itself down.
    vi.mocked(accessibilityGranted).mockReturnValue(true);
    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);

    // Run well past the 60 s budget — no timeout should fire.
    await vi.advanceTimersByTimeAsync(2000 * 40);
    expect(events).not.toContain('timeout');

    // Still ticking: a revoke that lands now is still observable.
    const callsSoFar = vi.mocked(refreshSettings).mock.calls.length;
    await vi.advanceTimersByTimeAsync(2000);
    expect(vi.mocked(refreshSettings).mock.calls.length).toBeGreaterThan(callsSoFar);
  });

  it('does not deliver a stale timeout to a new session that attached mid-fetch', async () => {
    // The 60 s tick fires the final fetch and awaits it. If the last subscriber
    // detaches and a brand-new session attaches before that fetch resolves, the
    // stale continuation must bail on the generation check instead of firing a
    // `timeout` at — or latching `everGranted` on — the new session.
    const final: { resolve: (() => void) | undefined } = { resolve: undefined };
    let fetchCount = 0;
    vi.mocked(refreshSettings).mockImplementation(() => {
      fetchCount += 1;
      // The 31st fetch is the one fired at t=60 s; hold it open so we can swap
      // sessions while it is in flight.
      if (fetchCount === 31) {
        return new Promise<void>((resolve) => {
          final.resolve = resolve;
        });
      }
      return Promise.resolve();
    });

    const eventsA: string[] = [];
    const unsubA = subscribeToPolling({ onEvent: (event) => eventsA.push(event) });
    await vi.advanceTimersByTimeAsync(0);
    // Advance to the 60 s tick; its final fetch is now pending.
    await vi.advanceTimersByTimeAsync(2000 * 30);
    expect(final.resolve).toBeDefined();

    // The last subscriber leaves and a fresh session attaches before the fetch
    // resolves.
    unsubA();
    const eventsB: string[] = [];
    subscribeToPolling({ onEvent: (event) => eventsB.push(event) });
    await vi.advanceTimersByTimeAsync(0);

    // Resolving the stale fetch must not leak a timeout into the new session.
    final.resolve?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(eventsB).not.toContain('timeout');
    expect(eventsA).not.toContain('timeout');
  });

  it('does not let a focus during the pending final fetch resume polling past a real timeout', async () => {
    // The timeout tick latches `timedOut` and stops the interval before the
    // final fetch resolves. A `focus` arriving while that fetch is pending
    // re-fetches once but must not restart the interval (the latch blocks it),
    // so when the fetch confirms no grant, polling does not run past the budget.
    const final: { resolve: (() => void) | undefined } = { resolve: undefined };
    let fetchCount = 0;
    vi.mocked(refreshSettings).mockImplementation(() => {
      fetchCount += 1;
      if (fetchCount === 31) {
        return new Promise<void>((resolve) => {
          final.resolve = resolve;
        });
      }
      return Promise.resolve();
    });

    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(2000 * 30);
    expect(final.resolve).toBeDefined();

    // A focus event arrives while the final fetch is still pending, restarting
    // the interval.
    window.dispatchEvent(new Event('focus'));
    await vi.advanceTimersByTimeAsync(0);

    // The final fetch resolves not-granted: the timeout must stop the restarted
    // interval.
    final.resolve?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(events).toContain('timeout');

    const callsAtTimeout = vi.mocked(refreshSettings).mock.calls.length;
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refreshSettings).toHaveBeenCalledTimes(callsAtTimeout);
  });

  it('re-fetches once but does not resume the interval on focus after a real timeout', async () => {
    // Once a real timeout has fired, refocusing the window should reflect the
    // latest state (a one-shot fetch) but must not silently resume continuous
    // polling past the budget — that needs an explicit recheck / new session.
    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(2000 * 30);
    expect(events).toContain('timeout');

    const callsAtTimeout = vi.mocked(refreshSettings).mock.calls.length;
    window.dispatchEvent(new Event('focus'));
    await vi.advanceTimersByTimeAsync(0);
    // One-shot fetch on focus...
    expect(refreshSettings).toHaveBeenCalledTimes(callsAtTimeout + 1);
    // ...but the interval stays torn down.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(refreshSettings).toHaveBeenCalledTimes(callsAtTimeout + 1);
  });

  it('re-enters revoke-watching when a post-timeout focus fetch observes a grant', async () => {
    // The user finally granted in System Settings and refocused the window
    // after the timeout. The focus fetch observes the grant, so the poller must
    // clear the timeout latch and resume polling to watch for a later revoke.
    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(2000 * 30);
    expect(events).toContain('timeout');

    vi.mocked(accessibilityGranted).mockReturnValue(true);
    window.dispatchEvent(new Event('focus'));
    await vi.advanceTimersByTimeAsync(0);

    // The grant restarts the interval (revoke-watching); no second timeout.
    const callsSoFar = vi.mocked(refreshSettings).mock.calls.length;
    await vi.advanceTimersByTimeAsync(2000 * 40);
    expect(vi.mocked(refreshSettings).mock.calls.length).toBeGreaterThan(callsSoFar);
    expect(events.filter((event) => event === 'timeout')).toHaveLength(1);
  });

  it('does not fire a stale timeout when a concurrent focus fetch already observed a grant', async () => {
    // The 60 s final fetch is held open. A `focus` then lands a grant
    // (foregroundFetch latches `everGranted` and restarts). When the older
    // timeout fetch finally resolves — committing a stale `denied` snapshot —
    // the `everGranted` latch must suppress the spurious timeout and keep
    // revoke-watching alive rather than tearing the interval down.
    const final: { resolve: (() => void) | undefined } = { resolve: undefined };
    let fetchCount = 0;
    vi.mocked(refreshSettings).mockImplementation(() => {
      fetchCount += 1;
      if (fetchCount === 31) {
        // The timeout's own fetch resolves last, with a stale denied snapshot.
        return new Promise<void>((resolve) => {
          final.resolve = () => {
            vi.mocked(accessibilityGranted).mockReturnValue(false);
            resolve();
          };
        });
      }
      return Promise.resolve();
    });

    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(2000 * 30);
    expect(final.resolve).toBeDefined();

    // A focus arrives and observes the grant while the timeout fetch is pending.
    vi.mocked(accessibilityGranted).mockReturnValue(true);
    window.dispatchEvent(new Event('focus'));
    await vi.advanceTimersByTimeAsync(0);

    // The stale timeout fetch resolves denied last — but `everGranted` is set.
    final.resolve?.();
    await vi.advanceTimersByTimeAsync(0);
    expect(events).not.toContain('timeout');

    // Revoke-watching continues.
    const callsSoFar = vi.mocked(refreshSettings).mock.calls.length;
    await vi.advanceTimersByTimeAsync(2000);
    expect(vi.mocked(refreshSettings).mock.calls.length).toBeGreaterThan(callsSoFar);
  });

  it('resumes revoke-watching when a recheck after a timeout observes a grant', async () => {
    // After a real timeout the interval is torn down. If the user then grants
    // in System Settings and the Setup tab issues a recheck, observing the
    // grant must re-arm polling so a later revoke on the same tab is still
    // detected — not leave the poller stuck stopped on the timeout latch.
    const events: string[] = [];
    subscribeToPolling({ onEvent: (event) => events.push(event) });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(2000 * 30);
    expect(events).toContain('timeout');

    // The grant lands and the user hits "Recheck".
    vi.mocked(accessibilityGranted).mockReturnValue(true);
    await refreshPermissionsOnce();

    // Polling resumes (revoke-watching); no further timeout.
    const callsSoFar = vi.mocked(refreshSettings).mock.calls.length;
    await vi.advanceTimersByTimeAsync(2000 * 5);
    expect(vi.mocked(refreshSettings).mock.calls.length).toBeGreaterThan(callsSoFar);
    expect(events.filter((event) => event === 'timeout')).toHaveLength(1);
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

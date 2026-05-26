// Accessibility permission helpers used by the Setup tab and the PermissionCard.
//
// The Setup card needs a single source of truth for the 5-state UI model and a
// single poller that drives backend re-fetches. Multiple Setup-aware surfaces
// (the Setup tab itself, the Capability table, a future PermissionCard in a
// detached window) can subscribe in parallel without each spinning up its own
// `setInterval` — a refcount keeps the timer alive while any subscriber is
// active and stops it when the last one detaches. The poller also pauses while
// the window is hidden / blurred so it does not burn IPC on a backgrounded
// Settings webview, and re-fetches once on `window.focus` so coming back from
// System Settings reflects a fresh grant immediately.

import { accessibilityGranted, refreshSettings } from '../stores/settings.svelte';
import { requestAccessibility as requestAccessibilityIpc } from './commands';
import { isTauri } from './tauri';
import type { OnboardingSettings, PermissionStatus, Platform } from './types';

// Frontend-derived UI state. Backend hands us `granted | denied | notDetermined
// | unsupported`; the Setup card needs a finer split that distinguishes
// `NotRequested` (we have not asked the OS yet) from `PromptShownNotGranted`
// (we asked once but the user dismissed/denied) and `RevokedAfterGranted` (a
// prior grant has since flipped off).
export type PermissionUiState =
  | 'NotRequested'
  | 'PromptShownNotGranted'
  | 'Granted'
  | 'RevokedAfterGranted'
  | 'Unavailable';

export const resolvePermissionUiState = (
  status: PermissionStatus | undefined,
  onboarding: OnboardingSettings | undefined,
  platform: Platform | undefined,
): PermissionUiState => {
  // Treat a missing capability snapshot the same as an unsupported platform
  // — there is nothing actionable for the user.
  if (platform === 'unsupported') return 'Unavailable';
  if (status === undefined) return 'NotRequested';
  if (status.state === 'unsupported') return 'Unavailable';
  if (status.state === 'granted') return 'Granted';
  // Non-macOS surfaces never drive the TCC dance: Windows reports a synthetic
  // granted (handled above) and the Wayland helper just reports denied when
  // `wtype` is missing. In either case the actionable copy is "install the
  // helper" / "no permission needed", not the macOS retry messaging.
  if (platform === 'windows' || platform === 'linuxWayland') return 'Unavailable';
  // A prior grant is the strongest signal: even if backend currently reports
  // `denied` or `notDetermined`, we know the user once trusted the app and is
  // most likely toggling it off intentionally / hit a TCC identity reset.
  if (onboarding?.accessibilityFirstGrantedAt != null) return 'RevokedAfterGranted';
  if (onboarding?.accessibilityPromptedAt != null) return 'PromptShownNotGranted';
  if (status.state === 'denied') return 'PromptShownNotGranted';
  return 'NotRequested';
};

// Thin IPC wrapper. Kept here (rather than calling `requestAccessibility`
// directly from the card) so test seams and error normalisation live in one
// place.
export const requestAccessibility = (prompt: boolean): Promise<PermissionStatus> =>
  requestAccessibilityIpc(prompt);

// Poller configuration. The interval is short enough that a grant via System
// Settings feels immediate (a focus event also forces an out-of-band fetch);
// the timeout caps the IPC budget when the user wanders off without finishing
// the flow.
const POLL_INTERVAL_MS = 2000;
const POLL_TIMEOUT_MS = 60_000;

export type PollerEvent = 'tick' | 'timeout';

type Subscriber = (event: PollerEvent) => void;

type PollerInternals = {
  subscribers: Set<Subscriber>;
  intervalId: ReturnType<typeof setInterval> | null;
  startedAt: number | null;
  // Monotonic session id, bumped each time a fresh polling session starts (the
  // first subscriber attaches). The async final fetch in `handleTimeoutFetch`
  // captures this before awaiting and re-checks it after, so a continuation
  // that resolves after the last subscriber left — and a new session attached —
  // cannot mutate the current session's state or fire a stale `timeout`.
  generation: number;
  // Set once a poll observes a live grant. The 60 s timeout only caps the
  // wait for the *first* grant; after a grant has been seen we keep polling
  // indefinitely (bounded by the visibility pause) so a later revoke is
  // detected while the Setup tab stays in focus, without forcing a refocus.
  everGranted: boolean;
  // Latched the moment `tick()` decides to time out — *before* the async final
  // fetch resolves — and stays set once a real timeout fires. While set, the
  // foreground focus/visibility handlers re-fetch once but do *not* restart the
  // steady interval, so a backgrounded-then-refocused window cannot resume
  // polling past the budget (and a restarted interval cannot hit 60 s again,
  // which would queue a duplicate timeout). Cleared only by a fresh session or
  // by a fetch that observes a grant (which returns us to revoke-watching).
  timedOut: boolean;
  visibilityHandler: (() => void) | null;
  blurHandler: (() => void) | null;
  focusHandler: (() => void) | null;
  // Snapshot the document/window references at start time so we can detach
  // listeners exactly once even if `globalThis.document` is replaced by a
  // jsdom reset between subscribe/unsubscribe.
  doc: Document | null;
  win: Window | null;
};

// Module-scoped state. A single poller drives every subscriber so concurrent
// Setup surfaces (Settings tab, future PermissionCard) share one IPC stream.
const poller: PollerInternals = {
  subscribers: new Set(),
  intervalId: null,
  startedAt: null,
  generation: 0,
  everGranted: false,
  timedOut: false,
  visibilityHandler: null,
  blurHandler: null,
  focusHandler: null,
  doc: null,
  win: null,
};

const isHidden = (): boolean => {
  if (typeof document === 'undefined') return false;
  return document.visibilityState === 'hidden';
};

const fireFetch = async (): Promise<void> => {
  try {
    await refreshSettings();
  } catch {
    // refreshSettings already routes errors into the store; silent here so a
    // transient IPC blip does not unwind the interval loop.
  }
};

// Single entry point for "a grant has been observed in this session": latch
// `everGranted`, clear any timeout latch, and resume the steady interval if a
// subscriber is still waiting and the window is visible. Every fetch site that
// can observe a grant (tick, opening fetch, recheck, focus, the 60 s final
// fetch) routes through here so revoke-watching always resumes consistently —
// notably after a timeout, where a later recheck/grant must re-arm polling so a
// subsequent revoke on the same Setup tab is still detected.
const enterRevokeWatching = (): void => {
  poller.everGranted = true;
  poller.timedOut = false;
  if (poller.intervalId === null && poller.subscribers.size > 0 && !isHidden()) {
    startInterval();
  }
};

// Handle the final fetch fired at the 60 s mark. The decision must wait for
// the fresh snapshot because `fireFetch()` resolves asynchronously — a
// synchronous re-read in `tick()` would still see the pre-fetch state. A grant
// that landed in this final window flips us into revoke-watching mode (keep
// polling, no timeout) rather than tearing the interval down, so a later
// revoke is still observable. Otherwise we give up and surface the timeout.
const handleTimeoutFetch = async (): Promise<void> => {
  // Capture the session before awaiting: if the last subscriber leaves and a
  // new session attaches while this fetch is in flight, bail so we neither
  // mutate the new session's latch nor deliver a stale `timeout` to it.
  const gen = poller.generation;
  await fireFetch();
  if (poller.generation !== gen) return;
  // Revoke-watching wins over timeout. Besides the fresh snapshot, honour the
  // session latch: a concurrent `foregroundFetch` (e.g. a `focus` that landed
  // while this fetch was pending) may have already observed the grant and
  // flipped `everGranted`. Even if this fetch's own (possibly older) snapshot
  // commits `denied` last, that latch means we are in revoke-watching mode and
  // must not fire a spurious timeout / tear the interval down.
  if (accessibilityGranted() || poller.everGranted) {
    enterRevokeWatching();
    return;
  }
  // A real timeout. `timedOut` was already latched by `tick()` before this
  // fetch began (so foreground handlers never restarted the interval); just
  // make sure the interval is down and surface the timeout. Authoritative for
  // "gave up" — only a fresh session or an observed grant resumes polling.
  stopInterval();
  notify('timeout');
};

// Foreground re-fetch fired by `focus` / `visibilitychange` resume. Coming back
// to the window should reflect a System-Settings toggle immediately, so we
// always take a one-shot fetch. Whether we resume the steady interval is
// decided *after* the snapshot lands (same async hazard as the timeout fetch):
// a grant re-enters revoke-watching mode (latch `everGranted`, clear any
// timeout, restart); otherwise we resume only while we have not already given
// up. Scoped to the session via `generation` so a continuation that resolves
// after the last subscriber left cannot restart a stale poller.
const foregroundFetch = async (): Promise<void> => {
  const gen = poller.generation;
  await fireFetch();
  if (poller.generation !== gen) return;
  if (poller.subscribers.size === 0 || isHidden()) return;
  if (accessibilityGranted()) {
    enterRevokeWatching();
    return;
  }
  if (!poller.timedOut && poller.intervalId === null) startInterval();
};

// A steady-tick fetch that latches `everGranted` once its own snapshot lands.
// `tick()` reads `accessibilityGranted()` synchronously off the *previous*
// fetch, so a grant observed by one tick is only latched at the next tick —
// leaving a one-interval window where a grant seen then revoked is missed.
// Latching post-await closes that window (and is monotonic within a session).
const fireFetchAndLatch = async (): Promise<void> => {
  const gen = poller.generation;
  await fireFetch();
  if (poller.generation !== gen) return;
  if (accessibilityGranted()) enterRevokeWatching();
};

const tick = (): void => {
  if (poller.startedAt === null) return;
  if (isHidden()) return; // paused — visibilitychange will resume us
  // Latch the "we've seen a grant" flag so the timeout below stops applying.
  // Checked every tick (not just once) so a grant landing mid-poll flips us
  // into revoke-watching mode even if a later tick observes it as false.
  if (accessibilityGranted()) poller.everGranted = true;
  const elapsed = Date.now() - poller.startedAt;
  if (elapsed >= POLL_TIMEOUT_MS && !poller.everGranted) {
    // Latch `timedOut` now, before the async final fetch resolves, so a
    // `focus`/`visibilitychange` arriving while it is pending cannot restart
    // the interval on a denied snapshot. Stop the steady-state interval too;
    // `handleTimeoutFetch` clears the latch and restarts only if the fresh
    // snapshot reveals a grant landed in the final window.
    poller.timedOut = true;
    void handleTimeoutFetch();
    stopInterval();
    return;
  }
  void fireFetchAndLatch();
  notify('tick');
};

const notify = (event: PollerEvent): void => {
  // Iterate over a snapshot so a handler that unsubscribes mid-loop does not
  // skip its peers.
  const snapshot = Array.from(poller.subscribers);
  for (const sub of snapshot) sub(event);
};

const startInterval = (): void => {
  if (poller.intervalId !== null) return;
  poller.startedAt = Date.now();
  poller.intervalId = setInterval(tick, POLL_INTERVAL_MS);
};

const stopInterval = (): void => {
  if (poller.intervalId !== null) {
    clearInterval(poller.intervalId);
    poller.intervalId = null;
  }
  poller.startedAt = null;
};

const attachVisibilityHooks = (): void => {
  if (typeof document === 'undefined' || typeof window === 'undefined') return;
  if (poller.doc !== null) return;
  poller.doc = document;
  poller.win = window;
  const onVisibility = (): void => {
    if (poller.subscribers.size === 0) return;
    if (isHidden()) {
      stopInterval();
    } else if (poller.intervalId === null) {
      // Coming back foreground: re-fetch immediately so the card reflects a
      // System-Settings toggle without waiting a full tick; `foregroundFetch`
      // decides whether to resume the steady interval once the snapshot lands.
      void foregroundFetch();
    }
  };
  const onBlur = (): void => {
    if (poller.subscribers.size === 0) return;
    stopInterval();
  };
  const onFocus = (): void => {
    if (poller.subscribers.size === 0) return;
    void foregroundFetch();
  };
  poller.visibilityHandler = onVisibility;
  poller.blurHandler = onBlur;
  poller.focusHandler = onFocus;
  poller.doc.addEventListener('visibilitychange', onVisibility);
  poller.win.addEventListener('blur', onBlur);
  poller.win.addEventListener('focus', onFocus);
};

const detachVisibilityHooks = (): void => {
  if (poller.doc !== null && poller.visibilityHandler !== null) {
    poller.doc.removeEventListener('visibilitychange', poller.visibilityHandler);
  }
  if (poller.win !== null && poller.blurHandler !== null) {
    poller.win.removeEventListener('blur', poller.blurHandler);
  }
  if (poller.win !== null && poller.focusHandler !== null) {
    poller.win.removeEventListener('focus', poller.focusHandler);
  }
  poller.doc = null;
  poller.win = null;
  poller.visibilityHandler = null;
  poller.blurHandler = null;
  poller.focusHandler = null;
};

export type SubscribeOptions = {
  // Called whenever a poll completes — `event` is `'tick'` for a normal IPC
  // round-trip and `'timeout'` when the 60 s budget elapses without a grant.
  onEvent?: Subscriber;
};

// Subscribe to the shared poller. Returns an `unsubscribe` callback. The
// poller is only started when the first subscriber attaches and stopped when
// the last one detaches; intermediate subscribe/unsubscribe pairs do not
// restart the timer (so the 60 s timeout countdown reflects the original
// window the user has been waiting, not the most recent subscription).
export const subscribeToPolling = (options: SubscribeOptions = {}): (() => void) => {
  const sub: Subscriber = (event) => options.onEvent?.(event);
  poller.subscribers.add(sub);
  if (poller.subscribers.size === 1) {
    // Fresh polling session: bump the generation so any in-flight timeout
    // continuation from a prior session bails, and reset the grant latch so
    // the 60 s timeout applies again to this session's wait for a (re-)grant.
    poller.generation += 1;
    poller.everGranted = false;
    poller.timedOut = false;
    attachVisibilityHooks();
    // Fire an immediate fetch so subscribers see fresh state before the first
    // interval tick lands. `fireFetchAndLatch` (not bare `fireFetch`) so a
    // grant in this opening snapshot latches `everGranted` for the session,
    // matching the tick fetch's invariant.
    void fireFetchAndLatch();
    if (!isHidden()) startInterval();
  }
  return () => {
    if (!poller.subscribers.delete(sub)) return;
    if (poller.subscribers.size === 0) {
      stopInterval();
      detachVisibilityHooks();
    }
  };
};

// Trigger a single fetch outside the interval. Used by callers that want a
// fresh snapshot in response to a UI event (e.g. the user pressed "Recheck"
// after a timeout) without having to also subscribe. Routes through
// `fireFetchAndLatch` so a grant observed by a recheck latches `everGranted`
// for any active session, the same invariant the tick / opening fetch hold.
export const refreshPermissionsOnce = async (): Promise<void> => {
  if (!isTauri()) return;
  await fireFetchAndLatch();
};

// Test-only: tear down all internal state so a fresh test doesn't inherit a
// previous test's interval / listeners. Not used in production code.
export const resetPollerForTests = (): void => {
  poller.subscribers.clear();
  poller.generation += 1;
  poller.everGranted = false;
  poller.timedOut = false;
  stopInterval();
  detachVisibilityHooks();
};

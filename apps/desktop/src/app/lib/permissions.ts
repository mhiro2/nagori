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

import { refreshSettings } from '../stores/settings.svelte';
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

const tick = (): void => {
  if (poller.startedAt === null) return;
  if (isHidden()) return; // paused — visibilitychange will resume us
  const elapsed = Date.now() - poller.startedAt;
  if (elapsed >= POLL_TIMEOUT_MS) {
    // Fire one last refresh so a grant that landed in the final window
    // surfaces as `Granted` (which clears the timedOut banner) instead of
    // forcing the user to click Re-check just to discover they already
    // succeeded.
    void fireFetch();
    notify('timeout');
    stopInterval();
    return;
  }
  void fireFetch();
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
      // Coming back foreground: take an immediate fetch so the card reflects
      // a System-Settings toggle without waiting a full tick.
      void fireFetch();
      startInterval();
    }
  };
  const onBlur = (): void => {
    if (poller.subscribers.size === 0) return;
    stopInterval();
  };
  const onFocus = (): void => {
    if (poller.subscribers.size === 0) return;
    void fireFetch();
    if (poller.intervalId === null && !isHidden()) startInterval();
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
    attachVisibilityHooks();
    // Fire an immediate fetch so subscribers see fresh state before the first
    // interval tick lands.
    void fireFetch();
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
// after a timeout) without having to also subscribe.
export const refreshPermissionsOnce = async (): Promise<void> => {
  if (!isTauri()) return;
  await fireFetch();
};

// Test-only: tear down all internal state so a fresh test doesn't inherit a
// previous test's interval / listeners. Not used in production code.
export const resetPollerForTests = (): void => {
  poller.subscribers.clear();
  stopInterval();
  detachVisibilityHooks();
};

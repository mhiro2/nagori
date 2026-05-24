// Shared hotkey-registration failure store. Subscribed at App-level so a
// startup-time emit (the backend can fail to bind the global shortcut
// before any window has mounted its listener) is caught no matter which
// window the user opens first, and so SettingsView / a future StatusBar
// can render from one source of truth. The backend also caches the
// latest failure on `AppState` — the watcher queries it via
// `last_hotkey_failure` to re-hydrate after the listener attaches past
// the live emit.

import { lastHotkeyFailure } from '../lib/commands';
import { TAURI_EVENTS, isTauri, subscribe } from '../lib/tauri';
import type { HotkeyFailure } from '../lib/types';

type HotkeyFailureState = {
  failure: HotkeyFailure | undefined;
};

export const hotkeyFailureState = $state<HotkeyFailureState>({ failure: undefined });

export const setHotkeyFailure = (failure: HotkeyFailure | undefined): void => {
  hotkeyFailureState.failure = failure;
};

export const dismissHotkeyFailure = (): void => {
  hotkeyFailureState.failure = undefined;
};

// Start the always-on watcher. Idempotent at the call site via the
// returned teardown: callers (App.svelte) start it in `onMount` and run
// the teardown in the unmount return. Three feeds shape the store:
//   1. The live `nagori://hotkey_register_failed` emit, for failures
//      that happen while the window is open.
//   2. The live `nagori://hotkey_register_resolved` emit, so a backend
//      success on a later reconcile drops the stale banner without
//      waiting for the user to dismiss it. Scoped by `kind` so a
//      primary success doesn't wipe a still-failing secondary, and by
//      `action` for secondaries so a sibling secondary resolving doesn't
//      drop a still-failing secondary's banner.
//   3. A `last_hotkey_failure` query fired after both subscriptions
//      report `onReady` — without that gate, a backend emit landing in
//      the window between `subscribe()` returning and `listen()`
//      actually attaching would both miss the live listener *and* land
//      in the cache too late for an eagerly-fired query to see, leaving
//      the user without any toast for that startup-time failure.
//
// Every cache read captures an epoch; any live event (failure or
// resolve) bumps the epoch and invalidates the in-flight read. Without
// that, a resolve interleaving with the initial hydration would let an
// older cache snapshot leak through — either surfacing an entry the
// backend just cleared, or (when the user has no banner yet) silently
// hiding a sibling secondary that is still failing. After every resolve
// we re-query so a still-cached sibling failure surfaces in place of
// the one that just resolved, and so a hydration that raced the resolve
// re-reads the post-resolve cache state.
export const startHotkeyFailureWatcher = (): (() => void) => {
  let cancelled = false;
  let attachedCount = 0;
  let hydrationEpoch = 0;

  const queryCache = (): void => {
    if (cancelled || !isTauri()) return;
    const epoch = ++hydrationEpoch;
    void lastHotkeyFailure()
      .then((cached) => {
        if (cancelled) return cached;
        // A live event since this query started invalidates the
        // snapshot. The cache contents we read may already be stale
        // relative to that live event — applying them would clobber
        // a fresh live failure or surface an entry the resolve just
        // cleared.
        if (epoch !== hydrationEpoch) return cached;
        if (cached && hotkeyFailureState.failure === undefined) {
          hotkeyFailureState.failure = cached;
        }
        return cached;
      })
      .catch(() => {
        // The cache is a best-effort fallback for a startup race; a
        // query failure (e.g. transient IPC blip) shouldn't blow up
        // the App shell. The live subscription still covers ongoing
        // failures.
        return null;
      });
  };

  const markAttached = (): void => {
    attachedCount += 1;
    if (attachedCount === 2) queryCache();
  };

  const offFailed = subscribe<HotkeyFailure>(
    TAURI_EVENTS.hotkeyRegisterFailed,
    (payload) => {
      if (!payload) return;
      hydrationEpoch += 1;
      const next: HotkeyFailure = {
        hotkey: payload.hotkey,
        error: payload.error,
      };
      if (payload.kind !== undefined) next.kind = payload.kind;
      if (payload.action !== undefined) next.action = payload.action;
      hotkeyFailureState.failure = next;
    },
    markAttached,
  );
  const offResolved = subscribe<{ kind?: string; action?: string }>(
    TAURI_EVENTS.hotkeyRegisterResolved,
    (payload) => {
      const current = hotkeyFailureState.failure;
      const resolvedKind = payload?.kind;
      if (current) {
        const kindMatches = (current.kind ?? undefined) === resolvedKind;
        // For secondaries the resolve is scoped to a specific action
        // so two simultaneously-failing secondaries don't drop each
        // other's banners. A primary resolve has no action
        // discriminator (the primary slot is single-slot).
        let actionMatches = true;
        if (resolvedKind === 'secondary') {
          const resolvedAction = payload?.action;
          actionMatches = (current.action ?? undefined) === resolvedAction;
        }
        if (kindMatches && actionMatches) {
          hotkeyFailureState.failure = undefined;
        }
      }
      // Always re-query the cache after a resolve. Two reasons:
      //   - When the displayed failure was cleared above, the backend
      //     keeps per-action cache entries so a sibling secondary may
      //     still be failing; surface it instead of an empty toast.
      //   - When a resolve interleaves with the initial hydration
      //     before any failure has been displayed, the in-flight cache
      //     query holds a snapshot that predates this resolve. The
      //     epoch bump here invalidates it and the fresh query reads
      //     the post-resolve cache state.
      queryCache();
    },
    markAttached,
  );
  return () => {
    cancelled = true;
    offFailed();
    offResolved();
  };
};

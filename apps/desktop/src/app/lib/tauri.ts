// Thin wrapper over `@tauri-apps/api`'s `invoke` so the rest of the app does
// not import Tauri internals directly. When running outside a Tauri WebView
// (e.g. `vite dev` in a regular browser), this falls back to a noop dispatcher
// that surfaces a structured error so callers can render a degraded UI.

import { invoke as tauriInvoke } from '@tauri-apps/api/core';

import type { CommandError } from './types';

type TauriInternals = {
  metadata?: { currentWindow?: { label?: string } };
  currentWindow?: { label?: string };
};

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals;
  }
}

export const isTauri = (): boolean =>
  typeof window !== 'undefined' && window.__TAURI_INTERNALS__ !== undefined;

// Resolve the synchronous label of the current Tauri webview, used by
// `App.svelte` to pick which route to mount without paying a
// dynamic-import await on every load. `__TAURI_METADATA__.currentWindow`
// is populated by Tauri 1.x; Tauri 2 exposes the label on
// `window.__TAURI_INTERNALS__.metadata.currentWindow` instead. Falling
// back to `undefined` keeps non-Tauri test / dev-browser contexts on
// the existing in-process `viewState` route.
export const currentWindowLabel = (): string | undefined => {
  if (typeof window === 'undefined') return undefined;
  const internals = window.__TAURI_INTERNALS__;
  return internals?.metadata?.currentWindow?.label ?? internals?.currentWindow?.label;
};

export class TauriBridgeError extends Error {
  readonly code: string;
  readonly recoverable: boolean;

  constructor(error: CommandError) {
    super(error.message);
    this.name = 'TauriBridgeError';
    this.code = error.code;
    this.recoverable = error.recoverable;
  }
}

const NOT_AVAILABLE: CommandError = {
  code: 'tauri.unavailable',
  message: 'Tauri runtime is not available in this context.',
  recoverable: false,
};

export const invoke = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => {
  if (!isTauri()) {
    throw new TauriBridgeError(NOT_AVAILABLE);
  }
  try {
    return await tauriInvoke<T>(command, args);
  } catch (raw) {
    throw new TauriBridgeError(normalizeError(raw));
  }
};

const normalizeError = (raw: unknown): CommandError => {
  if (typeof raw === 'object' && raw !== null) {
    const candidate = raw as Partial<CommandError>;
    if (typeof candidate.code === 'string' && typeof candidate.message === 'string') {
      return {
        code: candidate.code,
        message: candidate.message,
        recoverable: candidate.recoverable ?? false,
      };
    }
  }
  return {
    code: 'tauri.unknown',
    message: typeof raw === 'string' ? raw : 'Unknown Tauri error.',
    recoverable: false,
  };
};

// Event names emitted from the Rust side. Centralised so a typo on either
// side of the bridge is a single edit, not a treasure hunt.
export const TAURI_EVENTS = {
  navigate: 'nagori://navigate',
  // Emitted after the capture loop inserts a new clipboard entry. Payload:
  // `{ entryId: string }`.
  clipboardChanged: 'nagori://clipboard_changed',
  // Emitted when the capture loop drops a copy the built-in secret policy
  // refuses to store (OTP-shaped / fully-redacted under the default
  // `store_redacted`, or `secret_handling = block`). The StatusBar leaves a
  // dismissible chip so the user knows the copy was NOT saved. Payload:
  // `{ kind: 'secret_redacted_dropped' | 'secret_blocked', reasons: string[] }`.
  // Keep in lockstep with `CAPTURE_SKIPPED_EVENT` in `src-tauri/src/lib.rs`.
  captureSkipped: 'nagori://capture_skipped',
  pasteFailed: 'nagori://paste_failed',
  hotkeyRegisterFailed: 'nagori://hotkey_register_failed',
  // Emitted after a previously failed global-shortcut binds successfully
  // on a later reconcile. The frontend store uses this to drop the
  // stale failure banner/toast without waiting for a manual dismiss.
  // Payload mirrors the failure event's `kind` discriminator
  // (`{ kind: "secondary" }` for secondaries, empty object for primary)
  // so the store only clears when the resolved side matches the
  // currently displayed failure.
  hotkeyRegisterResolved: 'nagori://hotkey_register_resolved',
  // Broadcast after every persisted settings change. The Settings view
  // subscribes so an external mutation (the tray's "Pause capture"
  // toggle, another window, an IPC client) merges into the in-memory
  // view instead of being silently overwritten by the next full-snapshot
  // autosave. Wire shape: `AppSettings` (`get_settings` payload).
  settingsChanged: 'nagori://settings_changed',
  // Streaming AI action lifecycle, all request-scoped by `requestId`. A run
  // begins with `aiStarted`, streams `aiDelta` (appended text) and/or
  // `aiReplace` (full-snapshot reset), and ends with exactly one of `aiDone`,
  // `aiCancelled`, or `aiError`. Receivers discard events whose `requestId`
  // does not match the run they are rendering.
  aiStarted: 'nagori://ai/started',
  aiDelta: 'nagori://ai/delta',
  aiReplace: 'nagori://ai/replace',
  aiDone: 'nagori://ai/done',
  aiError: 'nagori://ai/error',
  aiCancelled: 'nagori://ai/cancelled',
} as const;

export type TauriEventName = (typeof TAURI_EVENTS)[keyof typeof TAURI_EVENTS];

// Subscribe to a Tauri event without leaking listeners across the dynamic
// import await. If the consumer cleans up before `listen()` resolves we
// immediately unsubscribe instead of pushing the late unlisten into a list
// the caller has already drained. `onReady` (when provided) fires *after* the
// underlying `listen()` has attached, so callers that gate follow-up work on a
// real attach signal (querying a backend cache for an emit that fired in the
// gap, opening a start gate) get a definite signal instead of guessing.
// `onError` fires if the dynamic import or `listen()` rejected — exactly one of
// the two runs (unless the consumer cleaned up first), so a gate that must not
// hang on attach failure can fail closed rather than wait forever. On error the
// listener is not attached, so the event will not be delivered.
// oxlint-disable-next-line no-unnecessary-type-parameters
export const subscribe = <T>(
  event: TauriEventName,
  handler: (payload: T) => void,
  onReady?: () => void,
  onError?: () => void,
): (() => void) => {
  if (!isTauri()) {
    onReady?.();
    return () => {};
  }
  let cancelled = false;
  let unlisten: (() => void) | undefined;
  void (async () => {
    try {
      const { listen } = await import('@tauri-apps/api/event');
      const off = await listen<T>(event, (e) => handler(e.payload));
      if (cancelled) {
        off();
        return;
      }
      unlisten = off;
      onReady?.();
    } catch {
      // The dynamic import or `listen()` failed. Signal the failure (unless the
      // consumer already cleaned up) so a gate awaiting this subscription can
      // fail closed instead of hanging or starting work with no listener.
      if (!cancelled) onError?.();
    }
  })();
  return () => {
    cancelled = true;
    unlisten?.();
  };
};

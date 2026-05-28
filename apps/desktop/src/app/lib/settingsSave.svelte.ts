import type { AppSettings } from './types';

export type SaveStatus = 'idle' | 'saving' | 'saved' | 'error';

export type SettingsSaveControllerOptions = {
  buildSnapshot: () => AppSettings;
  updateSettings: (snapshot: AppSettings) => Promise<unknown>;
  describeError: (err: unknown) => string;
  // How long the "Saved" pill lingers after a successful round-trip
  // before the header collapses back to `idle`.
  savedHoldMs?: number;
  // Cool-down between automatic retries after a failed save. Without this
  // a transient backend hiccup would strand the failed snapshot until the
  // next edit or unmount.
  retryDelayMs?: number;
  // Fires after a successful save so the parent can clear unrelated error
  // surfaces (e.g. the `loading` failure banner) without leaking those
  // refs into the controller.
  onSaveSuccess?: () => void;
};

const DEFAULT_SAVED_HOLD_MS = 1500;
const DEFAULT_RETRY_DELAY_MS = 5000;

/**
 * Encapsulates the SettingsView autosave state machine: debounce timers,
 * an in-flight save with last-write-wins follow-up, a cool-down retry
 * after a backend failure, and the baselines (`lastSentJson` /
 * `lastPersistedJson`) that the remote-merge path needs to keep in sync.
 *
 * The controller is a plain class so it can be tested in isolation; its
 * mutable `saveStatus` / `saveError` fields are Svelte runes-state so
 * the host component can mirror them into the header pill.
 */
export class SettingsSaveController {
  saveStatus = $state<SaveStatus>('idle');
  saveError = $state<string | undefined>(undefined);

  #destroyed = false;
  #hydrated = false;
  #inflight: Promise<void> | null = null;
  #queued = false;
  // Raised by `noteExternalMerge` when an external `settings_changed`
  // event lands while a save is in flight. The success/catch branches
  // use this to keep `lastPersistedJson` aligned with the merged remote
  // snapshot instead of rewinding it to the pre-merge dispatch, and the
  // finally hook fires a follow-up commit so the merged local state —
  // which may now diverge from the snapshot the backend just accepted —
  // actually reaches disk.
  #externalMergeDuringInflight = false;
  #pendingTimer: ReturnType<typeof setTimeout> | null = null;
  #savedTimer: ReturnType<typeof setTimeout> | null = null;
  #retryTimer: ReturnType<typeof setTimeout> | null = null;
  // The exact JSON payload that failed, captured at the moment of
  // failure. The retry re-sends this verbatim instead of re-reading
  // live state so a mid-typed hotkey accelerator can't leak into the
  // IPC during the cool-down window.
  #pendingRetryJson: string | null = null;
  // JSON-serialised form of the last payload we handed to
  // `updateSettings`, set *before* the IPC is dispatched. Used to
  // suppress idempotent IPC.
  #lastSentJson = '';
  // JSON-serialised form of the last payload the backend acknowledged.
  // Advances only inside the success branch of `updateSettings`. The
  // failure branch rewinds `lastSentJson` to this value so the cool-down
  // retry / unmount flush can re-send the payload the backend rejected.
  #lastPersistedJson = '';

  readonly #savedHoldMs: number;
  readonly #retryDelayMs: number;

  constructor(private readonly opts: SettingsSaveControllerOptions) {
    this.#savedHoldMs = opts.savedHoldMs ?? DEFAULT_SAVED_HOLD_MS;
    this.#retryDelayMs = opts.retryDelayMs ?? DEFAULT_RETRY_DELAY_MS;
  }

  /**
   * Mark the controller as ready and seed the JSON baselines so the
   * first commit attempt skips the no-op write and the unmount flush
   * stays quiet when the user only opened Settings to read.
   */
  hydrate(initialJson: string): void {
    this.#hydrated = true;
    this.#lastSentJson = initialJson;
    this.#lastPersistedJson = initialJson;
  }

  isHydrated(): boolean {
    return this.#hydrated;
  }

  /** True while an `updateSettings` round-trip is unresolved. */
  hasInflight(): boolean {
    return this.#inflight !== null;
  }

  /** True between a failed save and either its successful retry or a
   *  superseding edit. */
  hasPendingRetry(): boolean {
    return this.#pendingRetryJson !== null;
  }

  get persistedJson(): string {
    return this.#lastPersistedJson;
  }

  get sentJson(): string {
    return this.#lastSentJson;
  }

  /**
   * Handle a remote `settings_changed` echo (the backend confirming our
   * own most-recent dispatch). Returns `true` when the payload matches
   * the in-flight or just-sent snapshot; the caller can then skip the
   * full merge. Advances `lastPersistedJson` unless an external merge
   * has already done so during the in-flight window — clobbering it
   * with the pre-merge snapshot would let the next echo silently revert
   * the merge.
   */
  noteEcho(remoteJson: string): boolean {
    if (remoteJson !== this.#lastSentJson) return false;
    if (!this.#externalMergeDuringInflight) {
      this.#lastPersistedJson = remoteJson;
    }
    return true;
  }

  /**
   * Called by the host's `applyRemoteSettings` after a non-echo merge
   * has been applied to form state. Realigns the autosave baselines and
   * cancels any pending retry so a stale failed-payload retry can't
   * silently undo the just-applied remote mutation.
   */
  noteExternalMerge(remoteJson: string): void {
    this.#lastPersistedJson = remoteJson;
    if (this.#inflight === null) {
      this.#lastSentJson = remoteJson;
    } else {
      this.#externalMergeDuringInflight = true;
    }
    if (this.#pendingRetryJson !== null) {
      this.clearRetryTimer();
      if (this.#inflight === null) {
        this.scheduleSave(0);
      }
    }
  }

  clearRetryTimer(): void {
    if (this.#retryTimer !== null) {
      clearTimeout(this.#retryTimer);
      this.#retryTimer = null;
    }
    this.#pendingRetryJson = null;
  }

  scheduleSave(delay: number): void {
    if (!this.#hydrated || this.#destroyed) return;
    // A fresh user edit supersedes any cooled-down auto-retry. The edit
    // path will call `commitSave` anyway, so leaving the retry armed
    // would just produce a duplicate IPC moments later.
    this.clearRetryTimer();
    if (this.#pendingTimer !== null) {
      clearTimeout(this.#pendingTimer);
      this.#pendingTimer = null;
    }
    if (delay === 0) {
      void this.commitSave();
      return;
    }
    this.#pendingTimer = setTimeout(() => {
      this.#pendingTimer = null;
      void this.commitSave();
    }, delay);
  }

  /**
   * `overrideJson` is supplied by the retry timer to re-submit the
   * exact payload that failed earlier, bypassing the live-state read in
   * `buildSnapshot`. Without it the retry would pick up a mid-typed
   * hotkey accelerator from the textbox's two-way binding.
   */
  async commitSave(overrideJson?: string): Promise<void> {
    if (!this.#hydrated || this.#destroyed) return;

    if (this.#inflight) {
      // Full-snapshot semantics give us last-write-wins — a single
      // follow-up flag replaces a proper queue. The post-commit hook
      // re-invokes once and the latest snapshot wins.
      this.#queued = true;
      return;
    }

    // Skip a backend round-trip when the payload matches what we just
    // sent. The JSON round-trip also detaches the IPC payload from the
    // live `$state` proxy so a follow-up edit while `updateSettings`
    // is in flight can't mutate the snapshot mid-call.
    const snapshotJson = overrideJson ?? JSON.stringify(this.opts.buildSnapshot());
    if (snapshotJson === this.#lastSentJson) return;
    const snapshot: AppSettings = JSON.parse(snapshotJson);
    // Record the send *before* awaiting so a follow-up commit during
    // the in-flight window can short-circuit if it ends up emitting the
    // same payload.
    this.#lastSentJson = snapshotJson;

    this.saveStatus = 'saving';
    if (this.#savedTimer !== null) {
      clearTimeout(this.#savedTimer);
      this.#savedTimer = null;
    }

    this.#inflight = (async () => {
      this.#externalMergeDuringInflight = false;
      try {
        await this.opts.updateSettings(snapshot);
        // Advance the persisted baseline before the destroyed check so
        // a successful save that lands during teardown still updates
        // the record the unmount flush would otherwise re-send. Skip
        // when an external merge happened mid-flight: that merge has
        // already advanced `lastPersistedJson` to the merged remote
        // snapshot, and clobbering it with the pre-merge `snapshotJson`
        // here would let the next echo silently revert the merge.
        if (!this.#externalMergeDuringInflight) {
          this.#lastPersistedJson = snapshotJson;
        }
        if (this.#destroyed) return;
        // If another edit was already queued while we were in flight
        // skip the "Saved" pill — the next commit will flip the header
        // back to "Saving…" within the same tick anyway.
        if (!this.#queued) {
          this.saveStatus = 'saved';
          this.saveError = undefined;
          this.opts.onSaveSuccess?.();
          this.#savedTimer = setTimeout(() => {
            this.#savedTimer = null;
            if (this.saveStatus === 'saved') this.saveStatus = 'idle';
          }, this.#savedHoldMs);
        }
        // A retry timer left over from an earlier failure is now moot —
        // the most recent snapshot has landed.
        this.clearRetryTimer();
      } catch (err: unknown) {
        // Leave `lastPersistedJson` untouched and rewind `lastSentJson`
        // to it. The cool-down retry below and the unmount flush both
        // rebuild a snapshot from live state and compare against
        // `lastSentJson`; without the rewind the dedup short-circuit
        // at the top of `commitSave` would silently skip the retry of
        // the exact same payload.
        //
        // Apply the rewind unconditionally — including when an external
        // merge happened mid-flight. In that case `lastPersistedJson`
        // is the merged remote snapshot R; aligning `lastSentJson` to
        // R lets the follow-up commit in `finally` dispatch whenever
        // the merged live snapshot still diverges from R, and dedup
        // only when it doesn't.
        this.#lastSentJson = this.#lastPersistedJson;
        if (this.#destroyed) return;
        this.saveStatus = 'error';
        this.saveError = this.opts.describeError(err);
        if (this.#externalMergeDuringInflight) {
          // The follow-up commit triggered from `finally` will dispatch
          // the merged state; if it also fails its own catch branch
          // will arm a fresh retry. Re-arming the timer here would
          // double-fire and risks the pre-merge snapshot landing after
          // the merge follow-up.
          this.clearRetryTimer();
        } else {
          // Re-fire the save after a brief cool-down. Without this the
          // failed snapshot would be stranded until the user either
          // edits again or closes Settings — a transient IPC blip
          // would appear as a permanent error pill.
          this.clearRetryTimer();
          this.#pendingRetryJson = snapshotJson;
          this.#retryTimer = setTimeout(() => {
            this.#retryTimer = null;
            this.#fireRetry();
          }, this.#retryDelayMs);
        }
      }
    })();

    try {
      await this.#inflight;
    } finally {
      this.#inflight = null;
      // Drain order: a queued local edit and a pending external-merge
      // follow-up both want to fire a fresh commit. Either one alone
      // is enough — `commitSave` will rebuild from current state.
      const needsExternalMergeFollowUp = this.#externalMergeDuringInflight;
      this.#externalMergeDuringInflight = false;
      if ((this.#queued || needsExternalMergeFollowUp) && !this.#destroyed) {
        this.#queued = false;
        void this.commitSave();
      }
    }
  }

  /**
   * Send the captured retry payload, deferring if a save is already in
   * flight. Riding the `queued` drain instead would lose the override —
   * the drain calls `commitSave()` with no argument, which rebuilds the
   * snapshot from live state and could leak a mid-typed hotkey
   * accelerator.
   */
  #fireRetry(): void {
    if (this.#destroyed) return;
    const payload = this.#pendingRetryJson;
    if (payload === null) return;
    if (this.#inflight) {
      // `.finally` callbacks run in registration order: the outer
      // `await inflight` continuation fires first, so by the time our
      // handler runs the next `inflight` is either set (drain
      // started) or null. Re-evaluate from scratch.
      void this.#inflight.finally(() => {
        this.#fireRetry();
      });
      return;
    }
    void this.commitSave(payload);
  }

  /**
   * Tear down timers and mark the controller destroyed. Always safe to
   * call — the host runs this even when settings never hydrated, since
   * dangling timers would still fire post-unmount.
   */
  #disposeTimers(): void {
    if (this.#pendingTimer !== null) {
      clearTimeout(this.#pendingTimer);
      this.#pendingTimer = null;
    }
    if (this.#savedTimer !== null) {
      clearTimeout(this.#savedTimer);
      this.#savedTimer = null;
    }
    this.clearRetryTimer();
  }

  /**
   * Settings-window teardown hook. The host computes the final
   * snapshot (which depends on lastBlurred hotkeys and live form
   * state) and hands it here so the controller decides whether to
   * dispatch a final `updateSettings`, possibly after the in-flight
   * save settles. Always clears timers and marks itself destroyed,
   * even when `hydrated` never ran — a debounce timer armed before a
   * `get_settings` failure must still be cancelled.
   */
  flushOnUnmount(snapshotJson: string, snapshot: AppSettings): void {
    this.#disposeTimers();
    if (!this.#hydrated) {
      this.#destroyed = true;
      return;
    }
    // Swallow the error: the component is unmounting, so the status
    // pill is already gone and there's no surface left to render a
    // failure on. The next session reloads from disk anyway.
    const dispatchFinal = (): void => {
      void this.opts.updateSettings(snapshot).catch(() => {});
    };
    if (this.#inflight) {
      // Defer the decision until the in-flight save settles. Comparing
      // the live snapshot to `lastPersistedJson` at settle-time covers
      // every interleaving:
      //   • in-flight succeeds at the same payload → snapshot ==
      //     lastPersistedJson, skip (no duplicate IPC),
      //   • in-flight succeeds but the user reverted / followed up so
      //     the queued drain would have fired (gated off by
      //     `destroyed`) → snapshot != lastPersistedJson, dispatch,
      //   • in-flight fails entirely → its catch rewinds
      //     `lastSentJson` and bails on `destroyed` before arming the
      //     retry timer, leaving `lastPersistedJson` at the pre-edit
      //     baseline → snapshot != lastPersistedJson, dispatch (the
      //     only path left for the edit to survive).
      // Chaining off `.finally` instead of firing in parallel
      // serialises against the in-flight save: the backend's SQLite
      // pool uses multiple connections, so two parallel
      // `update_settings` could settle out of order.
      void this.#inflight.finally(() => {
        if (snapshotJson !== this.#lastPersistedJson) dispatchFinal();
      });
    } else if (snapshotJson !== this.#lastSentJson) {
      // No in-flight: an earlier failure may have rewound
      // `lastSentJson` to the persisted baseline (and we just cleared
      // its retry timer above), or this is the first save attempt.
      // Either way, snapshot ≠ lastSentJson means there's an unflushed
      // edit that needs to land.
      dispatchFinal();
    }
    this.#destroyed = true;
  }
}

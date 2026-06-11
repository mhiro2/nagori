import { afterEach, describe, expect, it, vi } from 'vitest';

import { SettingsSaveController, type SettingsSaveControllerOptions } from './settingsSave.svelte';
import type { AppSettings } from './types';

// Unit tests for the autosave state machine, driven directly against the
// controller. SettingsView.test.ts keeps integration specs proving the
// component wires its form controls into `scheduleSave` / `flushOnUnmount`;
// the save/queue/retry/baseline interleavings live here where they can use
// minimal snapshots and controlled promises instead of DOM round-trips.

// The controller only ever JSON-round-trips the snapshot, so a marker object
// stands in for the real settings shape.
const snap = (marker: string): AppSettings => ({ marker }) as unknown as AppSettings;
const json = (marker: string): string => JSON.stringify(snap(marker));

type Harness = {
  controller: SettingsSaveController;
  updateSettings: ReturnType<typeof vi.fn>;
  // Live form state the controller reads through `buildSnapshot`.
  setLive: (marker: string) => void;
};

const makeController = (opts: Partial<SettingsSaveControllerOptions> = {}): Harness => {
  let live = snap('base');
  const updateSettings = vi.fn(async () => {});
  const controller = new SettingsSaveController({
    buildSnapshot: () => live,
    updateSettings,
    describeError: (err) => String(err),
    ...opts,
  });
  controller.hydrate(json('base'));
  return {
    controller,
    updateSettings,
    setLive: (marker: string) => {
      live = snap(marker);
    },
  };
};

// Flush microtasks plus any timers due now without advancing the clock.
const settle = async (): Promise<void> => {
  await vi.advanceTimersByTimeAsync(0);
};

afterEach(() => {
  vi.useRealTimers();
});

describe('SettingsSaveController', () => {
  it('ignores schedule and commit before hydrate', async () => {
    const updateSettings = vi.fn(async () => {});
    const controller = new SettingsSaveController({
      buildSnapshot: () => snap('edit'),
      updateSettings,
      describeError: String,
    });
    controller.scheduleSave(0);
    await controller.commitSave();
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it('skips the IPC when the snapshot matches the last sent payload', async () => {
    const h = makeController();
    await h.controller.commitSave();
    expect(h.updateSettings).not.toHaveBeenCalled();
  });

  it('commits a changed snapshot and round-trips through saved back to idle', async () => {
    vi.useFakeTimers();
    const h = makeController();
    h.setLive('edit');
    await h.controller.commitSave();
    expect(h.updateSettings).toHaveBeenCalledTimes(1);
    expect(h.updateSettings.mock.calls[0]?.[0]).toEqual(snap('edit'));
    expect(h.controller.saveStatus).toBe('saved');
    expect(h.controller.persistedJson).toBe(json('edit'));
    // The "Saved" pill collapses after the hold.
    await vi.advanceTimersByTimeAsync(1500);
    expect(h.controller.saveStatus).toBe('idle');
  });

  it('coalesces a burst of debounced schedules into one IPC', async () => {
    vi.useFakeTimers();
    const h = makeController();
    h.setLive('edit-1');
    h.controller.scheduleSave(350);
    await vi.advanceTimersByTimeAsync(200);
    h.setLive('edit-2');
    h.controller.scheduleSave(350);
    await vi.advanceTimersByTimeAsync(350);
    expect(h.updateSettings).toHaveBeenCalledTimes(1);
    expect(h.updateSettings.mock.calls[0]?.[0]).toEqual(snap('edit-2'));
  });

  it('coalesces edits that arrive while a save is in flight', async () => {
    vi.useFakeTimers();
    let resolveFirst: (() => void) | undefined;
    const h = makeController();
    h.updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          resolveFirst = resolve;
        }),
    );
    h.setLive('edit-1');
    void h.controller.commitSave();
    await settle();
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    // Two follow-up edits while in flight collapse into the single
    // queued-drain commit, which rebuilds from the latest live state.
    h.setLive('edit-2');
    void h.controller.commitSave();
    h.setLive('edit-3');
    void h.controller.commitSave();
    await settle();
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    resolveFirst?.();
    await settle();
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('edit-3'));
  });

  it('retries the exact failed payload after the cool-down', async () => {
    vi.useFakeTimers();
    const h = makeController();
    h.updateSettings.mockRejectedValueOnce(new Error('backend transient'));
    h.setLive('edit');
    await h.controller.commitSave();
    expect(h.controller.saveStatus).toBe('error');
    expect(h.controller.hasPendingRetry()).toBe(true);

    // Mid-typed live state must not leak into the retry: the timer
    // re-sends the captured payload verbatim.
    h.setLive('half-typ');
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('edit'));
    expect(h.controller.hasPendingRetry()).toBe(false);
  });

  it('lets a follow-up edit re-send after a failure via the baseline rewind', async () => {
    const h = makeController();
    h.updateSettings.mockRejectedValueOnce(new Error('backend transient'));
    h.setLive('edit-1');
    await h.controller.commitSave();
    expect(h.controller.sentJson).toBe(json('base'));

    // The combined follow-up payload differs from the rewound baseline,
    // so the dedup short-circuit lets it through.
    h.setLive('edit-2');
    await h.controller.commitSave();
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('edit-2'));
  });

  it('cancels the pending retry when a fresh schedule lands', async () => {
    vi.useFakeTimers();
    const h = makeController();
    h.updateSettings.mockRejectedValueOnce(new Error('backend transient'));
    h.setLive('edit-1');
    await h.controller.commitSave();
    expect(h.controller.hasPendingRetry()).toBe(true);

    h.setLive('edit-2');
    h.controller.scheduleSave(0);
    await settle();
    expect(h.updateSettings).toHaveBeenCalledTimes(2);

    // The original cool-down deadline passes without a third IPC.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
  });

  it('does not fan out a third IPC when a retry collides with a queued drain', async () => {
    // Save A in flight, edit B queued behind it, A fails (arms the
    // retry). The retry must chain behind B's drain instead of joining
    // the queue, and B's success clears the pending retry — exactly two
    // IPCs leave the controller.
    vi.useFakeTimers();
    let rejectA: ((err: Error) => void) | undefined;
    let resolveB: (() => void) | undefined;
    const h = makeController();
    h.updateSettings
      .mockImplementationOnce(
        () =>
          new Promise<void>((_, reject) => {
            rejectA = reject;
          }),
      )
      .mockImplementationOnce(
        () =>
          new Promise<void>((resolve) => {
            resolveB = resolve;
          }),
      );

    h.setLive('edit-a');
    void h.controller.commitSave();
    await settle();
    h.setLive('edit-b');
    void h.controller.commitSave();
    await settle();
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    rejectA?.(new Error('backend transient'));
    await settle();
    expect(h.updateSettings).toHaveBeenCalledTimes(2);

    // Cool-down elapses while B is still in flight; the retry defers.
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.updateSettings).toHaveBeenCalledTimes(2);

    // B resolves; its success branch clears the pending retry, so the
    // chained fire bails without a third IPC.
    resolveB?.();
    await vi.advanceTimersByTimeAsync(100);
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
    expect(h.controller.hasPendingRetry()).toBe(false);
  });

  it('skips the unmount flush when nothing changed', async () => {
    const h = makeController();
    h.controller.flushOnUnmount(json('base'), snap('base'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.updateSettings).not.toHaveBeenCalled();
  });

  it('flushes an unsent edit on unmount', async () => {
    const h = makeController();
    h.controller.flushOnUnmount(json('edit'), snap('edit'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.updateSettings).toHaveBeenCalledTimes(1);
    expect(h.updateSettings.mock.calls[0]?.[0]).toEqual(snap('edit'));
  });

  it('re-sends a failed snapshot on unmount', async () => {
    // No retry button exists; the unmount flush compares against the
    // *persisted* baseline (the failure rewound `lastSentJson`), giving
    // the rejected payload one more shot on the way out.
    const h = makeController();
    h.updateSettings.mockRejectedValueOnce(new Error('backend transient'));
    h.setLive('edit');
    await h.controller.commitSave();
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    h.controller.flushOnUnmount(json('edit'), snap('edit'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('edit'));
  });

  it('defers the unmount flush until the in-flight save settles, then ships the divergence', async () => {
    // Two parallel update_settings could settle out of order on the
    // backend's connection pool; the flush must chain behind the
    // in-flight save. The revert case is the sharp edge: the in-flight
    // payload (edit) lands after the user reverted to base, so the
    // settle-time comparison against lastPersistedJson must dispatch
    // the corrective revert.
    let resolveFirst: (() => void) | undefined;
    const h = makeController();
    h.updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          resolveFirst = resolve;
        }),
    );
    h.setLive('edit');
    void h.controller.commitSave();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    // Revert, then unmount while the save is still pending.
    h.controller.flushOnUnmount(json('base'), snap('base'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    resolveFirst?.();
    await vi.waitFor(() => {
      expect(h.updateSettings).toHaveBeenCalledTimes(2);
    });
    // Disk ends at the user's intent: the revert, not the orphaned edit.
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('base'));
  });

  it('skips the deferred flush when the in-flight save persisted the same payload', async () => {
    let resolveFirst: (() => void) | undefined;
    const h = makeController();
    h.updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          resolveFirst = resolve;
        }),
    );
    h.setLive('edit');
    void h.controller.commitSave();
    await new Promise((resolve) => setTimeout(resolve, 0));

    h.controller.flushOnUnmount(json('edit'), snap('edit'));
    resolveFirst?.();
    await new Promise((resolve) => setTimeout(resolve, 0));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.updateSettings).toHaveBeenCalledTimes(1);
  });

  it('dispatches once more when the in-flight save fails after unmount', async () => {
    // The failure branch bails on `destroyed` before arming the retry
    // timer, so the chained flush is the only path keeping the edit
    // alive.
    let rejectFirst: ((err: Error) => void) | undefined;
    const h = makeController();
    h.updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((_, reject) => {
          rejectFirst = reject;
        }),
    );
    h.setLive('edit');
    void h.controller.commitSave();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(h.updateSettings).toHaveBeenCalledTimes(1);

    h.controller.flushOnUnmount(json('edit'), snap('edit'));
    rejectFirst?.(new Error('backend transient'));
    await vi.waitFor(() => {
      expect(h.updateSettings).toHaveBeenCalledTimes(2);
    });
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('edit'));
  });

  it('treats a matching remote payload as an echo and advances the persisted baseline', async () => {
    const h = makeController();
    h.setLive('edit');
    await h.controller.commitSave();
    expect(h.controller.noteEcho(json('edit'))).toBe(true);
    expect(h.controller.persistedJson).toBe(json('edit'));
    expect(h.controller.noteEcho(json('other'))).toBe(false);
  });

  it('realigns both baselines on an external merge while idle', async () => {
    const h = makeController();
    h.controller.noteExternalMerge(json('remote'));
    expect(h.controller.sentJson).toBe(json('remote'));
    expect(h.controller.persistedJson).toBe(json('remote'));

    // The merged state matches the baseline, so no IPC fires…
    h.setLive('remote');
    await h.controller.commitSave();
    expect(h.updateSettings).not.toHaveBeenCalled();

    // …until the user actually diverges from it.
    h.setLive('edit');
    await h.controller.commitSave();
    expect(h.updateSettings).toHaveBeenCalledTimes(1);
  });

  it('fires a follow-up commit when an external merge lands during an in-flight save', async () => {
    let resolveFirst: (() => void) | undefined;
    const h = makeController();
    h.updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          resolveFirst = resolve;
        }),
    );
    h.setLive('edit');
    void h.controller.commitSave();
    await new Promise((resolve) => setTimeout(resolve, 0));

    // The host merged remote state into the form mid-flight.
    h.controller.noteExternalMerge(json('merged'));
    h.setLive('merged-with-edit');

    resolveFirst?.();
    await vi.waitFor(() => {
      expect(h.updateSettings).toHaveBeenCalledTimes(2);
    });
    // The follow-up ships the merged live state, and the persisted
    // baseline kept the merge instead of rewinding to the pre-merge
    // payload the backend just acknowledged.
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('merged-with-edit'));
    expect(h.controller.persistedJson).toBe(json('merged-with-edit'));
  });

  it('keeps the merged baseline when a pre-merge echo arrives after the merge', async () => {
    let resolveFirst: (() => void) | undefined;
    const h = makeController();
    h.updateSettings.mockImplementationOnce(
      () =>
        new Promise<void>((resolve) => {
          resolveFirst = resolve;
        }),
    );
    h.setLive('edit');
    void h.controller.commitSave();
    await new Promise((resolve) => setTimeout(resolve, 0));

    h.controller.noteExternalMerge(json('merged'));
    // The backend's echo of our own pre-merge dispatch must not rewind
    // the persisted baseline to the pre-merge snapshot.
    expect(h.controller.noteEcho(json('edit'))).toBe(true);
    expect(h.controller.persistedJson).toBe(json('merged'));
    resolveFirst?.();
  });

  it('replaces a pending retry with a fresh commit on an external merge', async () => {
    vi.useFakeTimers();
    const h = makeController();
    h.updateSettings.mockRejectedValueOnce(new Error('backend transient'));
    h.setLive('edit');
    await h.controller.commitSave();
    expect(h.controller.hasPendingRetry()).toBe(true);

    // The merge cancels the armed retry (its payload predates the
    // merge) and immediately commits the merged live state instead.
    h.setLive('merged-with-edit');
    h.controller.noteExternalMerge(json('merged'));
    await settle();
    expect(h.controller.hasPendingRetry()).toBe(false);
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
    expect(h.updateSettings.mock.calls[1]?.[0]).toEqual(snap('merged-with-edit'));

    // The stale cool-down deadline passes without an extra IPC.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(h.updateSettings).toHaveBeenCalledTimes(2);
  });
});

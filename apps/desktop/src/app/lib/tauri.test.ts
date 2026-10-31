import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen, type EventCallback, type UnlistenFn } from '@tauri-apps/api/event';

import {
  TAURI_EVENTS,
  TauriBridgeError,
  currentWindowLabel,
  invoke,
  isTauri,
  subscribe,
} from './tauri';

const setTauriInternals = (value: unknown): void => {
  if (value === undefined) {
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    return;
  }
  (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = value;
};

beforeEach(() => {
  setTauriInternals(undefined);
});

afterEach(() => {
  setTauriInternals(undefined);
});

describe('isTauri', () => {
  it('returns false when __TAURI_INTERNALS__ is not present', () => {
    expect(isTauri()).toBe(false);
  });

  it('returns true once the runtime injects __TAURI_INTERNALS__', () => {
    setTauriInternals({});
    expect(isTauri()).toBe(true);
  });
});

describe('invoke', () => {
  it('throws TauriBridgeError when the runtime is unavailable', async () => {
    await expect(invoke('search_clipboard')).rejects.toBeInstanceOf(TauriBridgeError);
    expect(tauriInvoke).not.toHaveBeenCalled();
  });

  it('forwards command + args to @tauri-apps/api/core when in Tauri', async () => {
    setTauriInternals({});
    vi.mocked(tauriInvoke).mockResolvedValue('ok');
    const result = await invoke<string>('cmd', { foo: 1 });
    expect(result).toBe('ok');
    expect(tauriInvoke).toHaveBeenCalledWith('cmd', { foo: 1 });
  });

  it('wraps a structured CommandError-shaped reject in TauriBridgeError', async () => {
    setTauriInternals({});
    vi.mocked(tauriInvoke).mockRejectedValue({
      code: 'storage_error',
      message: 'disk full',
      recoverable: true,
    });
    try {
      await invoke('cmd');
      expect.unreachable('expected throw');
    } catch (err) {
      expect(err).toBeInstanceOf(TauriBridgeError);
      const bridgeErr = err as TauriBridgeError;
      expect(bridgeErr.code).toBe('storage_error');
      expect(bridgeErr.message).toBe('disk full');
      expect(bridgeErr.recoverable).toBe(true);
    }
  });

  it('defaults recoverable to false when the structured payload omits it', async () => {
    setTauriInternals({});
    vi.mocked(tauriInvoke).mockRejectedValue({ code: 'x', message: 'y' });
    await expect(invoke('cmd')).rejects.toMatchObject({
      code: 'x',
      message: 'y',
      recoverable: false,
    });
  });

  it('falls back to a generic tauri.unknown error for unexpected reject shapes', async () => {
    setTauriInternals({});
    vi.mocked(tauriInvoke).mockRejectedValue(42);
    await expect(invoke('cmd')).rejects.toMatchObject({
      code: 'tauri.unknown',
      message: 'Unknown Tauri error.',
      recoverable: false,
    });
  });

  it('uses the raw string when the reject value is a plain string', async () => {
    setTauriInternals({});
    vi.mocked(tauriInvoke).mockRejectedValue('boom');
    await expect(invoke('cmd')).rejects.toMatchObject({
      code: 'tauri.unknown',
      message: 'boom',
    });
  });
});

describe('currentWindowLabel', () => {
  it('is undefined outside the Tauri runtime', () => {
    expect(currentWindowLabel()).toBeUndefined();
  });

  it('reads the label Tauri 2 publishes in the internals metadata', () => {
    setTauriInternals({ metadata: { currentWindow: { label: 'settings' } } });
    expect(currentWindowLabel()).toBe('settings');
  });

  it('falls back to the legacy currentWindow slot', () => {
    setTauriInternals({ currentWindow: { label: 'main' } });
    expect(currentWindowLabel()).toBe('main');
  });

  it('is undefined when the runtime exposes no label', () => {
    setTauriInternals({});
    expect(currentWindowLabel()).toBeUndefined();
  });
});

describe('subscribe', () => {
  type Listener = EventCallback<unknown>;

  // Hold `listen()` open so a test decides when (and whether) the listener
  // attaches, and keep the callback it registered so payloads can be
  // delivered through the real wrapper.
  const deferListen = (): {
    attach: () => Promise<void>;
    fail: () => Promise<void>;
    deliver: (payload: unknown) => void;
    off: ReturnType<typeof vi.fn<UnlistenFn>>;
  } => {
    const off = vi.fn<UnlistenFn>();
    let resolveListen: ((value: UnlistenFn) => void) | undefined;
    let rejectListen: ((reason: unknown) => void) | undefined;
    let callback: Listener | undefined;
    vi.mocked(listen).mockImplementation((_event, handler) => {
      callback = handler;
      return new Promise<UnlistenFn>((resolve, reject) => {
        resolveListen = resolve;
        rejectListen = reject;
      });
    });
    // Let the wrapper's dynamic import and `listen()` call run before the
    // test settles the promise, then let its continuation run after.
    const settle = async (outcome: () => void): Promise<void> => {
      await vi.waitFor(() => {
        expect(resolveListen).toBeDefined();
      });
      outcome();
      await new Promise((resolve) => setTimeout(resolve, 0));
    };
    return {
      attach: () => settle(() => resolveListen?.(off)),
      fail: () => settle(() => rejectListen?.(new Error('listen failed'))),
      deliver: (payload) => {
        callback?.({ event: TAURI_EVENTS.clipboardChanged, id: 1, payload });
      },
      off,
    };
  };

  it('reports ready immediately and never listens outside the Tauri runtime', () => {
    const onReady = vi.fn();
    const unsubscribe = subscribe(TAURI_EVENTS.clipboardChanged, vi.fn(), onReady);
    expect(onReady).toHaveBeenCalledTimes(1);
    expect(listen).not.toHaveBeenCalled();
    unsubscribe();
  });

  it('reports ready only once the listener attaches, then delivers payloads', async () => {
    setTauriInternals({});
    const listener = deferListen();
    const handler = vi.fn();
    const onReady = vi.fn();
    const onError = vi.fn();

    const unsubscribe = subscribe(TAURI_EVENTS.clipboardChanged, handler, onReady, onError);
    await vi.waitFor(() => {
      expect(listen).toHaveBeenCalledWith('nagori://clipboard_changed', expect.any(Function));
    });
    expect(onReady).not.toHaveBeenCalled();

    await listener.attach();
    expect(onReady).toHaveBeenCalledTimes(1);
    expect(onError).not.toHaveBeenCalled();

    listener.deliver({ entryId: 'e1' });
    expect(handler).toHaveBeenCalledWith({ entryId: 'e1' });

    unsubscribe();
    expect(listener.off).toHaveBeenCalledTimes(1);
  });

  it('detaches a listener that resolves after the consumer already unsubscribed', async () => {
    setTauriInternals({});
    const listener = deferListen();
    const onReady = vi.fn();

    const unsubscribe = subscribe(TAURI_EVENTS.clipboardChanged, vi.fn(), onReady);
    unsubscribe();
    await listener.attach();

    // The late unlisten runs on arrival instead of leaking, and the consumer
    // that is already gone is not told the subscription is ready.
    expect(listener.off).toHaveBeenCalledTimes(1);
    expect(onReady).not.toHaveBeenCalled();
  });

  it('reports an attach failure through onError instead of onReady', async () => {
    setTauriInternals({});
    const listener = deferListen();
    const onReady = vi.fn();
    const onError = vi.fn();

    subscribe(TAURI_EVENTS.clipboardChanged, vi.fn(), onReady, onError);
    await listener.fail();

    expect(onError).toHaveBeenCalledTimes(1);
    expect(onReady).not.toHaveBeenCalled();
  });

  it('stays silent about an attach failure after the consumer unsubscribed', async () => {
    setTauriInternals({});
    const listener = deferListen();
    const onError = vi.fn();

    const unsubscribe = subscribe(TAURI_EVENTS.clipboardChanged, vi.fn(), undefined, onError);
    unsubscribe();
    await listener.fail();

    expect(onError).not.toHaveBeenCalled();
  });
});

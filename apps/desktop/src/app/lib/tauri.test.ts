import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke as tauriInvoke } from '@tauri-apps/api/core';

import { TauriBridgeError, invoke, isTauri } from './tauri';

const setTauriInternals = (value: unknown): void => {
  if (value === undefined) {
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    return;
  }
  (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = value;
};

beforeEach(() => {
  vi.clearAllMocks();
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

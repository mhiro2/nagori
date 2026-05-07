// Thin wrapper over `@tauri-apps/api`'s `invoke` so the rest of the app does
// not import Tauri internals directly. When running outside a Tauri WebView
// (e.g. `vite dev` in a regular browser), this falls back to a noop dispatcher
// that surfaces a structured error so callers can render a degraded UI.

import { invoke as tauriInvoke } from '@tauri-apps/api/core';

import type { CommandError } from './types';

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

export const isTauri = (): boolean =>
  typeof window !== 'undefined' && window.__TAURI_INTERNALS__ !== undefined;

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

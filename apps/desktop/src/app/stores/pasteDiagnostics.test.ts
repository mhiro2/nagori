import { afterEach, describe, expect, it } from 'vitest';

import {
  clearPasteDiagnostics,
  normalizePasteReason,
  pasteDiagnosticsState,
  recordPasteFailure,
} from './pasteDiagnostics.svelte';

afterEach(() => {
  clearPasteDiagnostics();
});

describe('normalizePasteReason', () => {
  it('passes through every known reason token', () => {
    for (const reason of [
      'accessibilityMissing',
      'toolMissing',
      'timeout',
      'synthUnsupported',
      'previousAppLost',
      'unknown',
    ] as const) {
      expect(normalizePasteReason(reason)).toBe(reason);
    }
  });

  it('falls back to unknown for an absent or unrecognised token', () => {
    expect(normalizePasteReason(undefined)).toBe('unknown');
    expect(normalizePasteReason('somethingNew')).toBe('unknown');
  });
});

describe('paste diagnostics store', () => {
  it('records and clears the last failure', () => {
    expect(pasteDiagnosticsState.failure).toBeNull();
    recordPasteFailure({ reason: 'toolMissing', message: 'no wtype', tool: 'wtype' });
    expect(pasteDiagnosticsState.failure).toEqual({
      reason: 'toolMissing',
      message: 'no wtype',
      tool: 'wtype',
    });
    clearPasteDiagnostics();
    expect(pasteDiagnosticsState.failure).toBeNull();
  });
});

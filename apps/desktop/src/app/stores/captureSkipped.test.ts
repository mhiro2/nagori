import { afterEach, describe, expect, it } from 'vitest';

import { captureSkippedState, clearCaptureSkip, recordCaptureSkip } from './captureSkipped.svelte';

afterEach(() => {
  clearCaptureSkip();
});

describe('capture skipped store', () => {
  it('records and clears a secret_redacted_dropped notice', () => {
    expect(captureSkippedState.notice).toBeNull();
    recordCaptureSkip({
      kind: 'secret_redacted_dropped',
      reasons: ['one_time_password_pattern'],
    });
    expect(captureSkippedState.notice).toEqual({
      kind: 'secret_redacted_dropped',
      reasons: ['one_time_password_pattern'],
    });
    clearCaptureSkip();
    expect(captureSkippedState.notice).toBeNull();
  });

  it('records a secret_blocked notice', () => {
    recordCaptureSkip({ kind: 'secret_blocked', reasons: [] });
    expect(captureSkippedState.notice).toEqual({ kind: 'secret_blocked', reasons: [] });
  });

  it('records an unknown kind from a newer daemon as a generic notice', () => {
    // A skip kind this frontend predates must still replace whatever notice
    // is showing — ignoring it would leave a stale message about an older
    // copy. The chip falls back to the generic secret line for it.
    recordCaptureSkip({ kind: 'secret_blocked', reasons: [] });
    recordCaptureSkip({ kind: 'something_new', reasons: ['x'] });
    expect(captureSkippedState.notice).toEqual({ kind: 'something_new', reasons: ['x'] });
  });

  it('ignores a malformed payload', () => {
    recordCaptureSkip(null);
    recordCaptureSkip(undefined);
    recordCaptureSkip('secret_blocked');
    recordCaptureSkip({});
    recordCaptureSkip({ kind: 42 });
    recordCaptureSkip({ kind: '' });
    expect(captureSkippedState.notice).toBeNull();
  });

  it('coerces a missing/garbled reasons field to an empty array', () => {
    recordCaptureSkip({ kind: 'secret_blocked' });
    expect(captureSkippedState.notice).toEqual({ kind: 'secret_blocked', reasons: [] });

    recordCaptureSkip({ kind: 'secret_blocked', reasons: 'not-an-array' });
    expect(captureSkippedState.notice).toEqual({ kind: 'secret_blocked', reasons: [] });

    recordCaptureSkip({ kind: 'secret_blocked', reasons: ['ok', 5, null, 'also_ok'] });
    expect(captureSkippedState.notice).toEqual({
      kind: 'secret_blocked',
      reasons: ['ok', 'also_ok'],
    });
  });
});

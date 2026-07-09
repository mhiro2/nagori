// Last capture the built-in secret policy refused to store, kept so the
// palette can leave a persistent StatusBar chip telling the user their copy
// was NOT saved (rather than silently dropping it). The daemon emits
// `nagori://capture_skipped` with a classified `kind` and the matched secret
// `reasons`; App.svelte records it here, the StatusBar reads it, and it is
// cleared on the next successful capture or by manual dismiss.

export type CaptureSkipNotice = {
  // Classified drop cause (`nagori_daemon::CaptureSkipKind::token()`):
  // `secret_redacted_dropped` (fully-redacted / OTP-shaped body under the
  // default `store_redacted`) or `secret_blocked` (`secret_handling = block`).
  // Kept as an open string so a skip kind added by a newer daemon still
  // records here — the chip's message falls back to the generic secret line
  // (selection keys off `reasons`, not `kind`), which stays accurate for any
  // "your copy was not stored" event, and dropping it instead would leave a
  // stale previous notice on screen.
  kind: string;
  // `SensitivityReason::token()` values, e.g. `one_time_password_pattern`.
  // Used to pick the OTP-specific vs generic secret message. Unknown tokens
  // ride along harmlessly (the StatusBar only checks for the OTP marker).
  reasons: string[];
};

type CaptureSkippedState = {
  notice: CaptureSkipNotice | null;
};

export const captureSkippedState = $state<CaptureSkippedState>({ notice: null });

// Narrow an untrusted wire payload into a `CaptureSkipNotice`, ignoring a
// malformed shape (non-object payload, missing/non-string `kind`). `reasons`
// coerces to a defensive string array so a missing/garbled field can't crash
// the chip's reason lookup.
export const recordCaptureSkip = (payload: unknown): void => {
  if (payload === null || typeof payload !== 'object') return;
  const { kind, reasons } = payload as { kind?: unknown; reasons?: unknown };
  if (typeof kind !== 'string' || kind.length === 0) return;
  const safeReasons = Array.isArray(reasons)
    ? reasons.filter((r): r is string => typeof r === 'string')
    : [];
  captureSkippedState.notice = { kind, reasons: safeReasons };
};

export const clearCaptureSkip = (): void => {
  captureSkippedState.notice = null;
};

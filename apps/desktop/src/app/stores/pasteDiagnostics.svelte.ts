// Last auto-paste failure, kept so the palette can leave a persistent
// diagnostic chip in the StatusBar instead of only flashing a toast. The
// daemon emits `nagori://paste_failed` with a classified `reason`; App.svelte
// records it here, the StatusBar reads it, and it is cleared on the next
// successful paste, on an Accessibility grant, or by manual dismiss.

import type { PasteFailureReason } from '../lib/types';

export type PasteDiagnostic = {
  // Classified reason (matches `nagori_core::PasteFailureReason::token()`).
  reason: PasteFailureReason;
  // Backend-curated detail message — the human-readable fallback shown in the
  // toast; the StatusBar prefers the localized per-reason hint.
  message: string;
  // Present only for `toolMissing` — the external tool to install (e.g. `wtype`).
  tool?: string;
};

type PasteDiagnosticsState = {
  failure: PasteDiagnostic | null;
};

export const pasteDiagnosticsState = $state<PasteDiagnosticsState>({ failure: null });

const PASTE_FAILURE_REASONS = [
  'accessibilityMissing',
  'toolMissing',
  'timeout',
  'synthUnsupported',
  'previousAppLost',
  'unknown',
] as const satisfies readonly PasteFailureReason[];

// Type predicate so the narrowing is proven, not asserted — `includes` runs
// against the widened `readonly string[]` view so an arbitrary token is a
// valid argument.
const isPasteFailureReason = (raw: string): raw is PasteFailureReason =>
  (PASTE_FAILURE_REASONS as readonly string[]).includes(raw);

/// Coerce an untrusted wire token into a known `PasteFailureReason`, falling
/// back to `unknown` for an absent or unrecognised value (older daemon, or a
/// reason this build predates).
export const normalizePasteReason = (raw: string | undefined): PasteFailureReason =>
  raw !== undefined && isPasteFailureReason(raw) ? raw : 'unknown';

export const recordPasteFailure = (failure: PasteDiagnostic): void => {
  pasteDiagnosticsState.failure = failure;
};

export const clearPasteDiagnostics = (): void => {
  pasteDiagnosticsState.failure = null;
};

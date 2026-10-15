// Localized wording for a classified auto-paste failure, shared by the status
// bar chip and the OS notification raised while the palette is hidden.

import type { PasteDiagnostic } from '../stores/pasteDiagnostics.svelte';
import type { Messages } from './i18n/locales/en';

export const pasteFailureHint = (
  failure: Pick<PasteDiagnostic, 'reason' | 'tool'>,
  status: Messages['status'],
): string => {
  const hint = status.pasteDiagnostics.hint;
  switch (failure.reason) {
    case 'toolMissing':
      return hint.toolMissing({ tool: failure.tool ?? status.pasteDiagnostics.toolFallback });
    case 'timeout':
      return hint.timeout;
    case 'synthUnsupported':
      return hint.synthUnsupported;
    case 'previousAppLost':
      return hint.previousAppLost;
    case 'clipboardChanged':
      return hint.clipboardChanged;
    case 'accessibilityMissing':
      return hint.accessibilityMissing;
    default:
      return hint.unknown;
  }
};

/// Notification title: whether the item still made it to the clipboard. Every
/// failure but `clipboardChanged` happened after the copy landed, so a manual
/// paste works; `clipboardChanged` means the clipboard now holds something
/// else and nothing of ours was pasted.
export const pasteFailureTitle = (
  failure: Pick<PasteDiagnostic, 'reason'>,
  status: Messages['status'],
): string =>
  failure.reason === 'clipboardChanged'
    ? status.pasteDiagnostics.notice.nothingPasted
    : status.pasteDiagnostics.notice.copiedNotPasted;

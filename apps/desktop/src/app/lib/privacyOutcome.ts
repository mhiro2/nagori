// What a sensitivity classification means for the user, in place of the raw
// classifier name. Public / Unknown rows have nothing to explain.

import type { Messages } from './i18n/locales/en';
import type { Sensitivity } from './types';

export type PrivacyOutcome = {
  // Short chip text for the result row and preview header.
  label: string;
  // One sentence on the consequence: what is masked, hidden or refused.
  description: string;
};

export const privacyOutcome = (
  sensitivity: Sensitivity,
  labels: Messages['privacyOutcome'],
): PrivacyOutcome | undefined => {
  switch (sensitivity) {
    case 'Secret':
      return labels.secret;
    case 'Private':
      return labels.private;
    case 'Blocked':
      return labels.blocked;
    default:
      return undefined;
  }
};

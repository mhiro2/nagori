// Helpers for surfacing why a result matched. The daemon returns the full
// `RankReason[]` per result; the UI condenses it into a single per-row chip
// (the strongest *match* signal) and a fully labelled list in the preview.

import type { Messages } from './i18n/locales/en';
import type { RankReason } from './types';

// Match-type reasons answer "why did this match the query"; the remaining
// reasons (Recent / FrequentlyUsed / Pinned) explain ordering, not the hit
// itself. Ordered strongest-first so the row chip shows the most precise
// signal when several stack (e.g. an exact hit is also a prefix + substring).
const MATCH_REASON_PRIORITY: readonly RankReason[] = [
  'ExactMatch',
  'PrefixMatch',
  'SubstringMatch',
  'FullTextMatch',
  'SemanticMatch',
  'NgramMatch',
];

/**
 * The single reason worth surfacing as the per-row chip: the strongest match
 * signal present. Returns `undefined` when the only reasons are boosts — most
 * notably the empty-query recent listing, whose rows are all `Recent` — so
 * those rows stay chip-free instead of every row carrying a redundant badge.
 */
export const primaryRankReason = (reasons: readonly RankReason[]): RankReason | undefined => {
  for (const reason of MATCH_REASON_PRIORITY) {
    if (reasons.includes(reason)) return reason;
  }
  return undefined;
};

/** Localised short label for a single `RankReason`. */
export const rankReasonLabel = (reason: RankReason, labels: Messages['rankReason']): string => {
  // A `Record` keyed by every variant so adding a `RankReason` is a compile
  // error here until it gains a label.
  const byReason: Record<RankReason, string> = {
    ExactMatch: labels.exact,
    PrefixMatch: labels.prefix,
    SubstringMatch: labels.substring,
    FullTextMatch: labels.fullText,
    NgramMatch: labels.fuzzy,
    SemanticMatch: labels.semantic,
    Recent: labels.recent,
    FrequentlyUsed: labels.frequent,
    Pinned: labels.pinned,
  };
  return byReason[reason];
};

/** Comma-free labelled list for the preview footer (order preserved). */
export const rankReasonLabels = (
  reasons: readonly RankReason[],
  labels: Messages['rankReason'],
): string[] => reasons.map((reason) => rankReasonLabel(reason, labels));

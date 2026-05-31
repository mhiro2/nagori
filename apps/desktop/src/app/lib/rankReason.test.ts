import { describe, expect, it } from 'vitest';

import { en } from './i18n/locales/en';
import { primaryRankReason, rankReasonLabel, rankReasonLabels } from './rankReason';

describe('primaryRankReason', () => {
  it('picks the strongest match reason when several stack', () => {
    expect(primaryRankReason(['SubstringMatch', 'PrefixMatch', 'ExactMatch'])).toBe('ExactMatch');
    expect(primaryRankReason(['NgramMatch', 'SubstringMatch'])).toBe('SubstringMatch');
  });

  it('surfaces semantic and fuzzy as their own reasons', () => {
    expect(primaryRankReason(['SemanticMatch'])).toBe('SemanticMatch');
    expect(primaryRankReason(['NgramMatch'])).toBe('NgramMatch');
  });

  it('returns undefined when only boost reasons are present (recent listing)', () => {
    expect(primaryRankReason(['Recent'])).toBeUndefined();
    expect(primaryRankReason(['Recent', 'FrequentlyUsed', 'Pinned'])).toBeUndefined();
    expect(primaryRankReason([])).toBeUndefined();
  });
});

describe('rankReasonLabel / rankReasonLabels', () => {
  it('maps every variant to its localised label', () => {
    expect(rankReasonLabel('ExactMatch', en.rankReason)).toBe('Exact');
    expect(rankReasonLabel('NgramMatch', en.rankReason)).toBe('Fuzzy');
    expect(rankReasonLabel('SemanticMatch', en.rankReason)).toBe('Semantic');
    expect(rankReasonLabel('FullTextMatch', en.rankReason)).toBe('Text');
    expect(rankReasonLabel('Pinned', en.rankReason)).toBe('Pinned');
  });

  it('preserves order when labelling a list', () => {
    expect(rankReasonLabels(['ExactMatch', 'Recent'], en.rankReason)).toEqual(['Exact', 'Recent']);
  });
});

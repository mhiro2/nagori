import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  DEFAULT_LOCALE,
  DEFAULT_PREFERENCE,
  LOCALE_PREFERENCES,
  SUPPORTED_LOCALES,
  dateLocaleTag,
  detectInitialLocale,
  i18nState,
  messages,
  setLocale,
} from './index.svelte';

beforeEach(() => {
  setLocale(DEFAULT_LOCALE);
});

afterEach(() => {
  vi.restoreAllMocks();
});

// navigator.languages is read-only in jsdom but `defineProperty` lets us
// simulate browser locale negotiation without polluting the global.
const stubLanguages = (langs: readonly string[] | undefined): void => {
  Object.defineProperty(navigator, 'languages', {
    configurable: true,
    get: () => langs,
  });
};

const stubLanguage = (lang: string): void => {
  Object.defineProperty(navigator, 'language', {
    configurable: true,
    get: () => lang,
  });
};

describe('setLocale', () => {
  it('updates i18nState and reflects on document.documentElement.lang', () => {
    setLocale('ja');
    expect(i18nState.locale).toBe('ja');
    expect(i18nState.preference).toBe('ja');
    expect(document.documentElement.lang).toBe('ja');
  });

  it('switches active dictionary returned by messages()', () => {
    setLocale('ko');
    expect(messages()).toBe(messages());
    // The Korean dictionary differs from the English one for at least one
    // user-visible string; that's enough to confirm we picked the right map.
    setLocale('en');
    const enTitle = messages().settings.title;
    setLocale('ko');
    expect(messages().settings.title).not.toBe(enTitle);
  });

  it('resolves the system preference via navigator.languages', () => {
    stubLanguages(['de-DE']);
    setLocale('system');
    expect(i18nState.preference).toBe('system');
    expect(i18nState.locale).toBe('de');
    expect(document.documentElement.lang).toBe('de');
  });

  it('falls back to the default locale when system negotiation finds no match', () => {
    stubLanguages(['xx-YY']);
    setLocale('system');
    expect(i18nState.preference).toBe('system');
    expect(i18nState.locale).toBe(DEFAULT_LOCALE);
  });
});

describe('dateLocaleTag', () => {
  it('returns the BCP-47 tag for the active locale', () => {
    setLocale('en');
    expect(dateLocaleTag()).toBe('en-US');
    setLocale('ja');
    expect(dateLocaleTag()).toBe('ja-JP');
    setLocale('ko');
    expect(dateLocaleTag()).toBe('ko-KR');
    setLocale('zh-Hans');
    expect(dateLocaleTag()).toBe('zh-CN');
    setLocale('zh-Hant');
    expect(dateLocaleTag()).toBe('zh-TW');
    setLocale('de');
    expect(dateLocaleTag()).toBe('de-DE');
    setLocale('fr');
    expect(dateLocaleTag()).toBe('fr-FR');
    setLocale('es');
    expect(dateLocaleTag()).toBe('es-ES');
  });
});

describe('detectInitialLocale', () => {
  it('matches en-* family to en', () => {
    stubLanguages(['en-GB', 'en-US']);
    expect(detectInitialLocale()).toBe('en');
  });

  it('matches ja-* to ja', () => {
    stubLanguages(['ja-JP']);
    expect(detectInitialLocale()).toBe('ja');
  });

  it('matches ko-* to ko', () => {
    stubLanguages(['ko-KR']);
    expect(detectInitialLocale()).toBe('ko');
  });

  it('matches de-* / fr-* / es-* to their language tag', () => {
    stubLanguages(['de-AT']);
    expect(detectInitialLocale()).toBe('de');
    stubLanguages(['fr-CA']);
    expect(detectInitialLocale()).toBe('fr');
    stubLanguages(['es-MX']);
    expect(detectInitialLocale()).toBe('es');
  });

  it('routes zh-Hant / zh-TW / zh-HK / zh-MO to zh-Hant', () => {
    stubLanguages(['zh-Hant-TW']);
    expect(detectInitialLocale()).toBe('zh-Hant');
    stubLanguages(['zh-TW']);
    expect(detectInitialLocale()).toBe('zh-Hant');
    stubLanguages(['zh-HK']);
    expect(detectInitialLocale()).toBe('zh-Hant');
    stubLanguages(['zh-MO']);
    expect(detectInitialLocale()).toBe('zh-Hant');
  });

  it('routes other zh-* (plain, Hans, CN, SG) to zh-Hans', () => {
    stubLanguages(['zh-CN']);
    expect(detectInitialLocale()).toBe('zh-Hans');
    stubLanguages(['zh-Hans-CN']);
    expect(detectInitialLocale()).toBe('zh-Hans');
    stubLanguages(['zh']);
    expect(detectInitialLocale()).toBe('zh-Hans');
    stubLanguages(['zh-SG']);
    expect(detectInitialLocale()).toBe('zh-Hans');
  });

  it('skips unknown candidates and tries the next preference', () => {
    stubLanguages(['xx-YY', 'ja-JP', 'en-US']);
    expect(detectInitialLocale()).toBe('ja');
  });

  it('falls back to navigator.language when languages is empty', () => {
    stubLanguages([]);
    stubLanguage('ko-KR');
    expect(detectInitialLocale()).toBe('ko');
  });

  it('returns the default locale when no preference matches', () => {
    stubLanguages(['xx-YY', 'yy-ZZ']);
    expect(detectInitialLocale()).toBe(DEFAULT_LOCALE);
  });
});

describe('SUPPORTED_LOCALES / LOCALE_PREFERENCES', () => {
  it('lists every concrete dictionary we ship', () => {
    expect(SUPPORTED_LOCALES.toSorted()).toEqual([
      'de',
      'en',
      'es',
      'fr',
      'ja',
      'ko',
      'zh-Hans',
      'zh-Hant',
    ]);
  });

  it('exposes `system` as the first persistable preference', () => {
    expect(LOCALE_PREFERENCES[0]).toBe('system');
    expect(LOCALE_PREFERENCES).toContain(DEFAULT_PREFERENCE);
  });
});

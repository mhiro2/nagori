import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  DEFAULT_LOCALE,
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

describe('setLocale', () => {
  it('updates i18nState and reflects on document.documentElement.lang', () => {
    setLocale('ja');
    expect(i18nState.locale).toBe('ja');
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
  });
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

  it('folds zh-* (Hans, Hant, plain) to zh-Hans', () => {
    stubLanguages(['zh-CN']);
    expect(detectInitialLocale()).toBe('zh-Hans');
    stubLanguages(['zh-Hant-TW']);
    expect(detectInitialLocale()).toBe('zh-Hans');
    stubLanguages(['zh']);
    expect(detectInitialLocale()).toBe('zh-Hans');
  });

  it('skips unknown candidates and tries the next preference', () => {
    stubLanguages(['de-DE', 'ja-JP', 'en-US']);
    expect(detectInitialLocale()).toBe('ja');
  });

  it('falls back to navigator.language when languages is empty', () => {
    stubLanguages([]);
    stubLanguage('ko-KR');
    expect(detectInitialLocale()).toBe('ko');
  });

  it('returns the default locale when no preference matches', () => {
    stubLanguages(['fr-FR', 'de-DE']);
    expect(detectInitialLocale()).toBe(DEFAULT_LOCALE);
  });
});

describe('SUPPORTED_LOCALES', () => {
  it('lists the four locales we ship', () => {
    expect(SUPPORTED_LOCALES.toSorted()).toEqual(['en', 'ja', 'ko', 'zh-Hans']);
  });
});

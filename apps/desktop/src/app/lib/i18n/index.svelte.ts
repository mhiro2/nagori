// Tiny dependency-free i18n layer for the palette UI. English is the base
// locale; additional dictionaries live in `locales/`. Every locale must
// structurally match `Messages`.

import { en, type Messages } from './locales/en';
import { ja } from './locales/ja';
import { ko } from './locales/ko';
import { zhHans } from './locales/zh-Hans';

export type Locale = 'en' | 'ja' | 'ko' | 'zh-Hans';

export const SUPPORTED_LOCALES: readonly Locale[] = ['en', 'ja', 'ko', 'zh-Hans'];

export const DEFAULT_LOCALE: Locale = 'en';

const MESSAGES: Record<Locale, Messages> = {
  en,
  ja,
  ko,
  'zh-Hans': zhHans,
};

const DATE_TAGS: Record<Locale, string> = {
  en: 'en-US',
  ja: 'ja-JP',
  ko: 'ko-KR',
  'zh-Hans': 'zh-CN',
};

export const i18nState = $state<{ locale: Locale }>({ locale: DEFAULT_LOCALE });

export const setLocale = (next: Locale): void => {
  i18nState.locale = next;
  if (typeof document !== 'undefined') {
    document.documentElement.lang = next;
  }
};

export const messages = (): Messages => MESSAGES[i18nState.locale];

export const dateLocaleTag = (): string => DATE_TAGS[i18nState.locale];

// All `zh-*` regional preferences fold to `zh-Hans` because that is currently
// the only Chinese variant we ship. If we add `zh-Hant` later, this needs to
// distinguish on the script subtag (`Hant`) before falling through.
const negotiateOne = (raw: string): Locale | undefined => {
  const lower = raw.toLowerCase();
  if (lower === 'en' || lower.startsWith('en-')) return 'en';
  if (lower === 'ja' || lower.startsWith('ja-')) return 'ja';
  if (lower === 'ko' || lower.startsWith('ko-')) return 'ko';
  if (lower === 'zh' || lower.startsWith('zh-') || lower.startsWith('zh_')) return 'zh-Hans';
  return undefined;
};

const negotiate = (preferences: readonly string[]): Locale => {
  for (const raw of preferences) {
    const tag = negotiateOne(raw);
    if (tag) return tag;
  }
  return DEFAULT_LOCALE;
};

export const detectInitialLocale = (): Locale => {
  if (typeof navigator === 'undefined') return DEFAULT_LOCALE;
  const candidates = navigator.languages?.length ? navigator.languages : [navigator.language];
  return negotiate(candidates ?? []);
};

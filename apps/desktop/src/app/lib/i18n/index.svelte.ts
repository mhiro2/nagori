// Tiny dependency-free i18n layer for the palette UI. English is the base
// locale; additional dictionaries live in `locales/`. Every locale must
// structurally match `Messages`.

import type { Locale as LocaleType, LocaleSetting } from '../types';
import { de } from './locales/de';
import { en, type Messages } from './locales/en';
import { es } from './locales/es';
import { fr } from './locales/fr';
import { ja } from './locales/ja';
import { ko } from './locales/ko';
import { zhHans } from './locales/zh-Hans';
import { zhHant } from './locales/zh-Hant';

export type Locale = LocaleType;
export type LocalePreference = LocaleSetting;

// Concrete locales we ship a dictionary for. `'system'` is *not* listed here
// — it's a persisted preference, not a dictionary key. See `LOCALE_PREFERENCES`.
export const SUPPORTED_LOCALES: readonly Locale[] = [
  'en',
  'ja',
  'ko',
  'zh-Hans',
  'zh-Hant',
  'de',
  'fr',
  'es',
];

// Values acceptable in `AppSettings.locale`. The dropdown in SettingsView
// renders this list verbatim so the `'system'` option is always first.
export const LOCALE_PREFERENCES: readonly LocalePreference[] = ['system', ...SUPPORTED_LOCALES];

export const DEFAULT_LOCALE: Locale = 'en';
export const DEFAULT_PREFERENCE: LocalePreference = 'system';

const MESSAGES: Record<Locale, Messages> = {
  en,
  ja,
  ko,
  'zh-Hans': zhHans,
  'zh-Hant': zhHant,
  de,
  fr,
  es,
};

const DATE_TAGS: Record<Locale, string> = {
  en: 'en-US',
  ja: 'ja-JP',
  ko: 'ko-KR',
  'zh-Hans': 'zh-CN',
  'zh-Hant': 'zh-TW',
  de: 'de-DE',
  fr: 'fr-FR',
  es: 'es-ES',
};

// `preference` is the value the user picked (and what's persisted). `locale`
// is the *resolved* dictionary key — equal to `preference` for concrete
// locales, or the negotiated OS-derived locale when `preference === 'system'`.
// All consumers read `locale`; `preference` is only used to round-trip the
// settings dropdown.
export const i18nState = $state<{ preference: LocalePreference; locale: Locale }>({
  preference: DEFAULT_PREFERENCE,
  locale: DEFAULT_LOCALE,
});

const resolve = (pref: LocalePreference): Locale =>
  pref === 'system' ? detectSystemLocale() : pref;

export const setLocale = (next: LocalePreference): void => {
  const resolved = resolve(next);
  i18nState.preference = next;
  i18nState.locale = resolved;
  if (typeof document !== 'undefined') {
    document.documentElement.lang = resolved;
  }
};

export const messages = (): Messages => MESSAGES[i18nState.locale];

export const dateLocaleTag = (): string => DATE_TAGS[i18nState.locale];

// `zh-*` preferences split on the script subtag: `Hant` → Traditional, every
// other `zh-*` (including the plain `zh` tag) → Simplified, since Simplified
// is the larger user base and the safer fallback when the OS only reports a
// region (`zh-CN`, `zh-SG`) without an explicit script.
const negotiateOne = (raw: string): Locale | undefined => {
  const lower = raw.toLowerCase();
  if (lower === 'en' || lower.startsWith('en-')) return 'en';
  if (lower === 'ja' || lower.startsWith('ja-')) return 'ja';
  if (lower === 'ko' || lower.startsWith('ko-')) return 'ko';
  if (lower === 'de' || lower.startsWith('de-')) return 'de';
  if (lower === 'fr' || lower.startsWith('fr-')) return 'fr';
  if (lower === 'es' || lower.startsWith('es-')) return 'es';
  if (lower === 'zh' || lower.startsWith('zh-') || lower.startsWith('zh_')) {
    // BCP-47 puts the script subtag right after the language tag (e.g.
    // `zh-Hant`, `zh-Hant-TW`). Region-only tags such as `zh-TW` / `zh-HK`
    // also conventionally mean Traditional in practice, so route them to
    // Traditional too.
    const normalized = lower.replace('_', '-');
    if (normalized.includes('-hant') || /-(tw|hk|mo)\b/.test(normalized)) return 'zh-Hant';
    return 'zh-Hans';
  }
  return undefined;
};

const negotiate = (preferences: readonly string[]): Locale => {
  for (const raw of preferences) {
    const tag = negotiateOne(raw);
    if (tag) return tag;
  }
  return DEFAULT_LOCALE;
};

// Read the OS / WebView language preferences and pick the best-matching
// concrete locale. Pulled into its own function so `setLocale('system')` can
// re-resolve without going through the public `detectInitialLocale` name,
// which is reserved for the first-paint hook in `main.ts`.
export const detectSystemLocale = (): Locale => {
  if (typeof navigator === 'undefined') return DEFAULT_LOCALE;
  const candidates = navigator.languages?.length ? navigator.languages : [navigator.language];
  return negotiate(candidates ?? []);
};

export const detectInitialLocale = (): Locale => detectSystemLocale();

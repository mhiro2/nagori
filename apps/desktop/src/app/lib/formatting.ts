// Pure formatting helpers used across the palette UI.

import { dateLocaleTag, messages } from './i18n/index.svelte';

const SECOND = 1_000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

// Intl.DateTimeFormat construction is one of V8's slower hot paths; cache one
// formatter per locale tag so result-row renders don't rebuild it.
const dateFormatterCache = new Map<string, Intl.DateTimeFormat>();
const dateFormatterFor = (tag: string): Intl.DateTimeFormat => {
  let fmt = dateFormatterCache.get(tag);
  if (!fmt) {
    fmt = new Intl.DateTimeFormat(tag);
    dateFormatterCache.set(tag, fmt);
  }
  return fmt;
};

export const formatRelativeTime = (isoTimestamp: string, now: Date = new Date()): string => {
  const target = new Date(isoTimestamp).getTime();
  if (Number.isNaN(target)) return '';
  const delta = now.getTime() - target;
  const t = messages().time;
  if (delta < MINUTE) return t.justNow;
  if (delta < HOUR) return t.minutesAgo(Math.floor(delta / MINUTE));
  if (delta < DAY) return t.hoursAgo(Math.floor(delta / HOUR));
  if (delta < 7 * DAY) return t.daysAgo(Math.floor(delta / DAY));
  return dateFormatterFor(dateLocaleTag()).format(target);
};

export const truncatePreview = (text: string, max: number = 120): string => {
  if (text.length <= max) return text;
  return `${text.slice(0, max - 1)}…`;
};

export const collapseWhitespace = (text: string): string => text.replaceAll(/\s+/g, ' ').trim();

export const formatByteCount = (bytes: number): string => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
};

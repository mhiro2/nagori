import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  collapseWhitespace,
  formatByteCount,
  formatRelativeTime,
  truncatePreview,
} from './formatting';
import { DEFAULT_LOCALE, setLocale } from './i18n/index.svelte';

describe('formatRelativeTime', () => {
  const now = new Date('2026-05-05T12:00:00Z');
  const SECOND = 1_000;
  const MINUTE = 60 * SECOND;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;
  const ago = (ms: number): string => new Date(now.getTime() - ms).toISOString();

  beforeEach(() => {
    setLocale('en');
  });

  afterEach(() => {
    setLocale(DEFAULT_LOCALE);
  });

  // Each row pins both the number and the unit, including the deltas on
  // either side of every unit boundary.
  it.each([
    [0, 'just now'],
    [59 * SECOND, 'just now'],
    [MINUTE, '1 min ago'],
    [5 * MINUTE, '5 min ago'],
    [HOUR - 1, '59 min ago'],
    [HOUR, '1 hr ago'],
    [3 * HOUR, '3 hr ago'],
    [DAY - 1, '23 hr ago'],
    [DAY, '1 day ago'],
    [4 * DAY, '4 days ago'],
    [7 * DAY - 1, '6 days ago'],
  ])('formats a %i ms delta as %s', (delta, expected) => {
    expect(formatRelativeTime(ago(delta), now)).toBe(expected);
  });

  it('formats the units with the active locale dictionary', () => {
    setLocale('ja');
    expect(formatRelativeTime(ago(30 * SECOND), now)).toBe('今');
    expect(formatRelativeTime(ago(5 * MINUTE), now)).toBe('5分前');
    expect(formatRelativeTime(ago(3 * HOUR), now)).toBe('3時間前');
    expect(formatRelativeTime(ago(4 * DAY), now)).toBe('4日前');
  });

  it('switches to a calendar date once the delta reaches a week', () => {
    expect(formatRelativeTime(ago(7 * DAY), now)).toBe(
      new Intl.DateTimeFormat('en-US').format(now.getTime() - 7 * DAY),
    );
  });

  it('falls back to a locale date for older timestamps', () => {
    const ts = new Date('2024-01-15T08:00:00Z').toISOString();
    const out = formatRelativeTime(ts, now);
    expect(out).not.toBe('');
    expect(out).toMatch(/2024/);
  });

  it('returns the empty string for an invalid timestamp', () => {
    expect(formatRelativeTime('not-a-date', now)).toBe('');
  });
});

describe('truncatePreview', () => {
  it('returns the original string when under the limit', () => {
    expect(truncatePreview('short', 10)).toBe('short');
  });

  it('appends an ellipsis when truncating', () => {
    const out = truncatePreview('abcdefghij', 5);
    expect(out).toHaveLength(5);
    expect(out.endsWith('…')).toBe(true);
  });

  it('uses the default limit of 120 characters', () => {
    const long = 'a'.repeat(200);
    expect(truncatePreview(long)).toHaveLength(120);
  });
});

describe('collapseWhitespace', () => {
  it('collapses runs of whitespace into single spaces', () => {
    expect(collapseWhitespace('hello   world')).toBe('hello world');
  });

  it('treats tabs and newlines as whitespace', () => {
    expect(collapseWhitespace('a\tb\nc\r\nd')).toBe('a b c d');
  });

  it('trims leading and trailing whitespace', () => {
    expect(collapseWhitespace('  surrounded  ')).toBe('surrounded');
  });
});

describe('formatByteCount', () => {
  it('uses bytes under 1 KiB', () => {
    expect(formatByteCount(0)).toBe('0 B');
    expect(formatByteCount(1023)).toBe('1023 B');
  });

  it('uses kilobytes between 1 KiB and 1 MiB', () => {
    expect(formatByteCount(1024)).toBe('1.0 KB');
    expect(formatByteCount(2048)).toBe('2.0 KB');
  });

  it('uses megabytes once at or above 1 MiB', () => {
    expect(formatByteCount(1024 * 1024)).toBe('1.0 MB');
    expect(formatByteCount(5 * 1024 * 1024)).toBe('5.0 MB');
  });
});

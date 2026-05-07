import { describe, expect, it } from 'vitest';

import {
  collapseWhitespace,
  formatByteCount,
  formatRelativeTime,
  truncatePreview,
} from './formatting';

describe('formatRelativeTime', () => {
  const now = new Date('2026-05-05T12:00:00Z');

  it('returns the just-now label for sub-minute deltas', () => {
    const ts = new Date(now.getTime() - 30_000).toISOString();
    expect(formatRelativeTime(ts, now)).toMatch(/just now|たった今/i);
  });

  it('returns minutes-ago for sub-hour deltas', () => {
    const ts = new Date(now.getTime() - 5 * 60_000).toISOString();
    expect(formatRelativeTime(ts, now)).toContain('5');
  });

  it('returns hours-ago for sub-day deltas', () => {
    const ts = new Date(now.getTime() - 3 * 60 * 60_000).toISOString();
    expect(formatRelativeTime(ts, now)).toContain('3');
  });

  it('returns days-ago when within the past week', () => {
    const ts = new Date(now.getTime() - 4 * 24 * 60 * 60_000).toISOString();
    expect(formatRelativeTime(ts, now)).toContain('4');
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

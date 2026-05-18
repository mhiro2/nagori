import { describe, expect, it } from 'vitest';

import {
  MAX_USER_REGEX_LEN,
  MAX_USER_REGEX_NESTING,
  maxParenNesting,
  validateUserRegex,
  validateUserRegexList,
} from './policyValidation';

describe('maxParenNesting', () => {
  it('counts flat groups as depth one', () => {
    expect(maxParenNesting('(foo|bar)')).toBe(1);
  });

  it('ignores escaped parens', () => {
    // Mirrors the Rust `user_regex_escaped_parens_do_not_count_toward_nesting`
    // case — users must be able to write literal `\(INTERNAL-\d+\)`.
    expect(maxParenNesting('\\(INTERNAL-\\d+\\)')).toBe(0);
  });

  it('tracks the deepest unescaped nesting', () => {
    expect(maxParenNesting('((((a))))')).toBe(4);
  });
});

describe('validateUserRegex', () => {
  it('accepts the realistic patterns the Rust suite covers', () => {
    // Lock parity with `user_regex_compiles_within_budget` in policy.rs.
    expect(validateUserRegex('INTERNAL-\\d+', 0)).toBeNull();
    expect(validateUserRegex('(?i)acme[_-]?[a-z0-9]{8,}', 1)).toBeNull();
  });

  it('rejects a pattern exceeding the byte cap with the backend message', () => {
    const long = 'a'.repeat(MAX_USER_REGEX_LEN + 1);
    const err = validateUserRegex(long, 0);
    expect(err?.kind).toBe('too_long');
    // Message has to track the Rust message verbatim — `save_settings`
    // surfaces the same text via `invalid_input`, and the UI test asserts
    // we never diverge from that string.
    expect(err?.message).toBe(`regex_denylist entry exceeds ${MAX_USER_REGEX_LEN}-byte limit`);
    expect(err?.detail.byteLength).toBe(MAX_USER_REGEX_LEN + 1);
  });

  it('rejects patterns nested past the policy limit', () => {
    // Same shape the backend `user_regex_deep_nesting_rejected` test uses.
    const pattern =
      '(' + '('.repeat(MAX_USER_REGEX_NESTING) + 'a' + ')'.repeat(MAX_USER_REGEX_NESTING) + ')';
    const err = validateUserRegex(pattern, 0);
    expect(err?.kind).toBe('too_nested');
    expect(err?.detail.nesting).toBe(MAX_USER_REGEX_NESTING + 1);
  });

  it('flags malformed regex syntax', () => {
    const err = validateUserRegex('(', 0);
    expect(err?.kind).toBe('invalid_syntax');
    expect(err?.message).toMatch(/^invalid regex_denylist entry/);
  });

  it('reports the index of the offending entry', () => {
    const errors = validateUserRegexList(['ok', '(']);
    expect(errors).toHaveLength(1);
    expect(errors[0]?.index).toBe(1);
  });

  it('accepts the user-style fixtures the docs advertise', () => {
    // Patterns that real users write — internal IDs, ticket URLs, in-house
    // token prefixes. Verifying these here keeps the UX claim ("the example
    // patterns in the help text work") honest when the limits move.
    expect(validateUserRegex('PROJ-\\d{4,6}', 0)).toBeNull();
    expect(validateUserRegex('https://example\\.atlassian\\.net/browse/[A-Z]+-\\d+', 1)).toBeNull();
    expect(validateUserRegex('acme_(?:live|test)_[a-z0-9]{16,}', 2)).toBeNull();
  });
});

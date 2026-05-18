// Client-side preflight for the `regex_denylist` textarea. Mirrors the
// limits enforced by `nagori_core::policy::compile_user_regex` so users
// get an actionable inline error before the round-trip to the backend
// surfaces a single opaque `invalid_input` string.
//
// The byte-length and paren-nesting checks are exact ports of the Rust
// implementation; the syntax probe uses the host `RegExp` constructor and
// is intentionally a soft signal — the Rust `regex` crate and ECMAScript
// `RegExp` share most of the surface a privacy rule needs (alternation,
// character classes, quantifiers, anchors, common escapes) but diverge on
// look-around and Unicode property syntax. A pattern that fails here is
// almost certainly broken; a pattern that passes here can still be
// rejected at save time, which is why the backend remains the source of
// truth.

export const MAX_USER_REGEX_LEN = 256;
export const MAX_USER_REGEX_NESTING = 3;

export type UserRegexErrorKind = 'too_long' | 'too_nested' | 'invalid_syntax' | 'empty';

export type UserRegexError = {
  kind: UserRegexErrorKind;
  // 0-based index into the textarea's non-blank lines, matching the order
  // `linesToList` produces for the save payload. The UI uses this to point
  // at the offending entry.
  index: number;
  pattern: string;
  // Backend-style message ("regex_denylist entry exceeds 256-byte limit",
  // …) — kept so the inline error can stay byte-accurate with the policy
  // error returned by `save_settings` if the user proceeds anyway.
  message: string;
  // Extra context (current value, threshold) used by the i18n layer to
  // build the user-facing string.
  detail: { byteLength?: number; nesting?: number; syntaxError?: string };
};

// Mirrors `nagori_core::policy::max_paren_nesting`. ASCII-only — the
// regex DSL's metacharacters are all 7-bit so multi-byte UTF-8 inside a
// literal or character class cannot perturb the count.
export const maxParenNesting = (pattern: string): number => {
  let depth = 0;
  let maxDepth = 0;
  let i = 0;
  while (i < pattern.length) {
    const ch = pattern[i];
    if (ch === '\\') {
      // Skip the escaped character so `\(` / `\)` don't count.
      i += 2;
      continue;
    }
    if (ch === '(') {
      depth += 1;
      if (depth > maxDepth) maxDepth = depth;
    } else if (ch === ')') {
      depth = depth > 0 ? depth - 1 : 0;
    }
    i += 1;
  }
  return maxDepth;
};

const byteLength = (s: string): number => new TextEncoder().encode(s).length;

// Matches `(?<flags>)` / `(?<flags>:...)` groups that the backend `regex`
// crate supports but ECMAScript `RegExp` does not. The negative class
// rules out JS-native constructs that also start with `(?`: non-capturing
// (`(?:`), lookaround (`(?=`, `(?!`), and named/lookbehind (`(?<`).
const PROBE_INCOMPATIBLE = /\(\?[^:<=!]/;

export const validateUserRegex = (pattern: string, index: number): UserRegexError | null => {
  if (pattern.length === 0) {
    return {
      kind: 'empty',
      index,
      pattern,
      message: 'regex_denylist entry is empty',
      detail: {},
    };
  }
  const bytes = byteLength(pattern);
  if (bytes > MAX_USER_REGEX_LEN) {
    return {
      kind: 'too_long',
      index,
      pattern,
      message: `regex_denylist entry exceeds ${MAX_USER_REGEX_LEN}-byte limit`,
      detail: { byteLength: bytes },
    };
  }
  const nesting = maxParenNesting(pattern);
  if (nesting > MAX_USER_REGEX_NESTING) {
    return {
      kind: 'too_nested',
      index,
      pattern,
      message: `regex_denylist entry has nesting depth ${nesting} (limit ${MAX_USER_REGEX_NESTING}); reduce parenthesised groups`,
      detail: { nesting },
    };
  }
  // Soft syntax check — `RegExp` diverges from the backend regex engine on
  // inline flag groups (`(?i)`, `(?-i:…)`, `(?im)`) and a handful of
  // Unicode property escapes. Skip the probe whenever the pattern uses
  // those constructs so we don't reject a valid backend rule; the backend
  // remains the source of truth either way.
  if (PROBE_INCOMPATIBLE.test(pattern)) {
    return null;
  }
  try {
    void new RegExp(pattern);
  } catch (err) {
    return {
      kind: 'invalid_syntax',
      index,
      pattern,
      message: `invalid regex_denylist entry ${JSON.stringify(pattern)}: ${
        err instanceof Error ? err.message : String(err)
      }`,
      detail: { syntaxError: err instanceof Error ? err.message : String(err) },
    };
  }
  return null;
};

export const validateUserRegexList = (patterns: readonly string[]): UserRegexError[] => {
  const errors: UserRegexError[] = [];
  patterns.forEach((pattern, index) => {
    const err = validateUserRegex(pattern, index);
    if (err) errors.push(err);
  });
  return errors;
};

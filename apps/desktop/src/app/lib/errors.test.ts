import { beforeEach, describe, expect, it } from 'vitest';

import { describeError } from './errors';
import { messages, setLocale } from './i18n/index.svelte';

beforeEach(() => {
  setLocale('en');
});

// Codes are part of the Rust error contract — any drift between the backend
// and these branches results in users seeing the bare message instead of a
// localized hint, so the table is pinned by name.
const t = (): ReturnType<typeof messages>['errors'] => messages().errors;

describe('describeError', () => {
  const codes: Array<[string, () => string]> = [
    ['storage_error', () => t().storage],
    ['search_error', () => t().search],
    ['platform_error', () => t().platform],
    ['permission_error', () => t().permission],
    ['ai_error', () => t().ai],
    ['policy_error', () => t().policy],
    ['not_found', () => t().notFound],
    ['configuration_error', () => t().configuration],
  ];

  for (const [code, expected] of codes) {
    it(`localizes the ${code} code`, () => {
      expect(describeError({ code, message: 'raw' })).toBe(expected());
    });
  }

  // `unsupported` deliberately prefers the backend-curated message (e.g.
  // "auto-update is only available on macOS", "Linux Wayland has no
  // Accessibility settings pane …") because the generic translation
  // loses the *why*. The localized label is the fallback when no
  // curated message is available.
  it('prefers the backend message for the unsupported code', () => {
    expect(describeError({ code: 'unsupported', message: 'macOS only' })).toBe('macOS only');
  });

  it('falls back to the localized label for unsupported when message is missing', () => {
    expect(describeError({ code: 'unsupported' })).toBe(t().unsupported);
    expect(describeError({ code: 'unsupported', message: '' })).toBe(t().unsupported);
  });

  // `invalid_input` follows the same shape as `unsupported`: the backend
  // attaches actionable specifics (the regex_denylist limit messages from
  // `compile_user_regex` being the canonical case), so squashing them to
  // "Invalid input." hid the *why* the user's save was rejected.
  it('prefers the backend message for the invalid_input code', () => {
    expect(
      describeError({
        code: 'invalid_input',
        message: 'regex_denylist entry exceeds 256-byte limit',
      }),
    ).toBe('regex_denylist entry exceeds 256-byte limit');
  });

  it('falls back to the localized label for invalid_input when message is missing', () => {
    expect(describeError({ code: 'invalid_input' })).toBe(t().invalidInput);
    expect(describeError({ code: 'invalid_input', message: '' })).toBe(t().invalidInput);
  });

  // `internal_error` is the one code whose backend message is built from raw
  // OS detail (absolute install paths, updater feed URLs, symlink targets), so
  // it must NEVER surface the message — always the generic localized string,
  // even when a message is attached.
  it('never surfaces the raw message for the internal_error code', () => {
    expect(
      describeError({ code: 'internal_error', message: 'failed to replace /Users/me/.local/bin' }),
    ).toBe(t().internal);
    expect(describeError({ code: 'internal_error' })).toBe(t().internal);
  });

  // `forbidden` messages are static curated strings composed by the command
  // handler (e.g. the Public-only preview gate), so they are safe to surface.
  it('prefers the backend message for the forbidden code', () => {
    expect(
      describeError({
        code: 'forbidden',
        message: 'expanded preview is only available for Public entries',
      }),
    ).toBe('expanded preview is only available for Public entries');
  });

  it('falls back to the localized label for forbidden when message is missing', () => {
    expect(describeError({ code: 'forbidden' })).toBe(t().forbidden);
    expect(describeError({ code: 'forbidden', message: '' })).toBe(t().forbidden);
  });

  // `paste_error` carries an actionable, already-curated hint with no
  // path/SQL detail, so the message is safe to surface verbatim.
  it('prefers the backend message for the paste_error code', () => {
    expect(
      describeError({
        code: 'paste_error',
        message: 'auto-paste failed: install the `wtype` package',
      }),
    ).toBe('auto-paste failed: install the `wtype` package');
  });

  it('falls back to the localized label for paste_error when message is missing', () => {
    expect(describeError({ code: 'paste_error' })).toBe(t().paste);
    expect(describeError({ code: 'paste_error', message: '' })).toBe(t().paste);
  });

  it('falls back to the raw message for unrecognised codes', () => {
    expect(describeError({ code: 'something_else', message: 'fallback msg' })).toBe('fallback msg');
  });

  it('falls back to the unknown label when the message field is missing', () => {
    expect(describeError({ code: 'something_else' })).toBe(t().unknown);
  });

  it('returns Error.message for thrown Error instances', () => {
    expect(describeError(new Error('boom'))).toBe('boom');
  });

  it('returns the raw string when the input is a string', () => {
    expect(describeError('plain string')).toBe('plain string');
  });

  it('returns the unknown label for any other value', () => {
    expect(describeError(undefined)).toBe(t().unknown);
    expect(describeError(null)).toBe(t().unknown);
    expect(describeError(42)).toBe(t().unknown);
  });

  it('honours the active locale', () => {
    setLocale('ja');
    expect(describeError({ code: 'storage_error' })).toBe(t().storage);
  });
});

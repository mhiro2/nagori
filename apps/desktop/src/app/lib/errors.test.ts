import { beforeEach, describe, expect, it } from 'vitest';

import { describeError } from './errors';
import { messages, setLocale } from './i18n/index.svelte';

beforeEach(() => {
  setLocale('en');
});

describe('describeError', () => {
  // Codes are part of the Rust error contract — any drift between the backend
  // and these branches results in users seeing the bare message instead of a
  // localized hint, so the table is pinned by name.
  const t = (): ReturnType<typeof messages>['errors'] => messages().errors;
  const codes: Array<[string, () => string]> = [
    ['storage_error', () => t().storage],
    ['search_error', () => t().search],
    ['platform_error', () => t().platform],
    ['permission_error', () => t().permission],
    ['ai_error', () => t().ai],
    ['policy_error', () => t().policy],
    ['not_found', () => t().notFound],
    ['invalid_input', () => t().invalidInput],
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

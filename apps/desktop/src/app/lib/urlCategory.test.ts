import { describe, expect, it } from 'vitest';

import { domainCategory } from './urlCategory';

describe('domainCategory', () => {
  it('labels well-known brands by registrable domain', () => {
    expect(domainCategory('github.com')).toBe('GitHub');
    expect(domainCategory('youtube.com')).toBe('YouTube');
    expect(domainCategory('youtu.be')).toBe('YouTube');
    expect(domainCategory('stackoverflow.com')).toBe('Stack Overflow');
    expect(domainCategory('notion.so')).toBe('Notion');
    expect(domainCategory('x.com')).toBe('X');
  });

  it('matches subdomains of a branded registrable domain', () => {
    expect(domainCategory('gist.github.com')).toBe('GitHub');
    expect(domainCategory('www.youtube.com')).toBe('YouTube');
    expect(domainCategory('raw.githubusercontent.com')).toBe('GitHub');
  });

  it('prefers the most specific Google property over the bare domain', () => {
    expect(domainCategory('docs.google.com')).toBe('Google Docs');
    expect(domainCategory('drive.google.com')).toBe('Google Drive');
    expect(domainCategory('mail.google.com')).toBe('Gmail');
    // A google.com host that is not one of the scoped properties falls back
    // to the generic brand rather than mislabelling it as Docs/Drive.
    expect(domainCategory('www.google.com')).toBe('Google');
  });

  it('is case-insensitive and tolerates a trailing FQDN dot', () => {
    expect(domainCategory('GitHub.com')).toBe('GitHub');
    expect(domainCategory('github.com.')).toBe('GitHub');
  });

  it('returns undefined for unknown or empty hosts', () => {
    expect(domainCategory('example.com')).toBeUndefined();
    expect(domainCategory('intranet.local')).toBeUndefined();
    expect(domainCategory('')).toBeUndefined();
    expect(domainCategory(undefined)).toBeUndefined();
    expect(domainCategory(null)).toBeUndefined();
  });

  it('does not match a brand that is only a substring of another host', () => {
    // `notgithub.com` ends with `github.com` only as a raw substring, not as
    // a dot-delimited subdomain, so it must not be labelled GitHub.
    expect(domainCategory('notgithub.com')).toBeUndefined();
  });
});

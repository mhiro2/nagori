import { describe, expect, it } from 'vitest';

import { additionalClipboardCategories, hasAccompanyingImage } from './representations';
import type { RepresentationSummary } from './types';

const rep = (mimeType: string, role: RepresentationSummary['role']): RepresentationSummary => ({
  mimeType,
  role,
  byteCount: 0,
});

describe('hasAccompanyingImage', () => {
  it('is true when a non-primary image rides along the primary content', () => {
    expect(
      hasAccompanyingImage([rep('text/uri-list', 'primary'), rep('image/png', 'alternative')]),
    ).toBe(true);
  });

  it('ignores the primary representation so an image-kind entry is not flagged', () => {
    // The image is the entry's own content, not a secondary render.
    expect(hasAccompanyingImage([rep('image/png', 'primary')])).toBe(false);
  });

  it('is false when no representation is an image', () => {
    expect(
      hasAccompanyingImage([rep('text/html', 'primary'), rep('text/plain', 'plainFallback')]),
    ).toBe(false);
  });

  it('ignores image formats the thumbnail pipeline cannot decode', () => {
    // SVG / HEIC are not in the daemon's allow-list, so flagging them would
    // request a /thumb the generator can never produce.
    expect(
      hasAccompanyingImage([rep('text/uri-list', 'primary'), rep('image/svg+xml', 'alternative')]),
    ).toBe(false);
    expect(
      hasAccompanyingImage([rep('text/uri-list', 'primary'), rep('image/heic', 'alternative')]),
    ).toBe(false);
  });

  it('is false for missing or empty summaries', () => {
    expect(hasAccompanyingImage(undefined)).toBe(false);
    expect(hasAccompanyingImage([])).toBe(false);
  });
});

describe('additionalClipboardCategories', () => {
  it('lists the extra formats beyond the primary kind, de-duplicated', () => {
    expect(
      additionalClipboardCategories([
        rep('text/uri-list', 'primary'),
        rep('image/png', 'alternative'),
        rep('text/plain', 'alternative'),
      ]),
    ).toEqual(['image', 'text']);
  });

  it('excludes the primary representation category', () => {
    expect(
      additionalClipboardCategories([
        rep('image/png', 'primary'),
        rep('image/jpeg', 'alternative'),
      ]),
    ).toEqual([]);
  });
});

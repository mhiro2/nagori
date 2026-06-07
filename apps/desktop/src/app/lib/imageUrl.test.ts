import { afterEach, describe, expect, it, vi } from 'vitest';

import { buildImageUrl } from './imageUrl';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('buildImageUrl', () => {
  it('requests the thumbnail endpoint on the custom scheme by default', () => {
    vi.stubGlobal('navigator', { userAgent: 'jsdom' });
    expect(buildImageUrl('abc', true, 0)).toBe('nagori-image://localhost/thumb/abc');
  });

  it('requests the full payload when not using the thumbnail', () => {
    vi.stubGlobal('navigator', { userAgent: 'jsdom' });
    expect(buildImageUrl('abc', false, 0)).toBe('nagori-image://localhost/abc');
  });

  it('switches to the http origin on Windows / Android so the Origin matches', () => {
    vi.stubGlobal('navigator', { userAgent: 'Mozilla/5.0 (Windows NT 10.0)' });
    expect(buildImageUrl('abc', true, 0)).toBe('http://nagori-image.localhost/thumb/abc');
  });

  it('appends a cache-busting suffix only on retry attempts', () => {
    vi.stubGlobal('navigator', { userAgent: 'jsdom' });
    expect(buildImageUrl('abc', true, 2)).toBe('nagori-image://localhost/thumb/abc?v=2');
  });
});

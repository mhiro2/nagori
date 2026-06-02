import { describe, expect, it } from 'vitest';

import { isScreenshotSource } from './screenshotSource';

describe('isScreenshotSource', () => {
  it('recognises common screenshot tools across platforms', () => {
    expect(isScreenshotSource('Screenshot')).toBe(true);
    expect(isScreenshotSource('screencapture')).toBe(true);
    expect(isScreenshotSource('CleanShot X')).toBe(true);
    expect(isScreenshotSource('Shottr')).toBe(true);
    expect(isScreenshotSource('Snipping Tool')).toBe(true);
    expect(isScreenshotSource('Flameshot')).toBe(true);
    expect(isScreenshotSource('Spectacle')).toBe(true);
    expect(isScreenshotSource('ShareX')).toBe(true);
  });

  it('is case-insensitive', () => {
    expect(isScreenshotSource('CLEANSHOT X')).toBe(true);
    expect(isScreenshotSource('flameshot')).toBe(true);
  });

  it('does not flag ordinary apps or missing names', () => {
    expect(isScreenshotSource('Safari')).toBe(false);
    expect(isScreenshotSource('Finder')).toBe(false);
    expect(isScreenshotSource('')).toBe(false);
    expect(isScreenshotSource(undefined)).toBe(false);
    expect(isScreenshotSource(null)).toBe(false);
  });
});

import { describe, expect, it } from 'vitest';

import { fileExtensionBadge } from './fileSummary';

describe('fileExtensionBadge', () => {
  it('uppercases a known short extension regardless of input case', () => {
    expect(fileExtensionBadge('quarterly-report.pptx')).toBe('PPTX');
    expect(fileExtensionBadge('photo.JPG')).toBe('JPG');
  });

  it('returns undefined for unknown or column-breaking extensions', () => {
    expect(fileExtensionBadge('archive.weirdext')).toBeUndefined();
    expect(fileExtensionBadge('model.safetensors')).toBeUndefined();
  });

  it('returns undefined for dotfiles and extensionless names', () => {
    expect(fileExtensionBadge('.env')).toBeUndefined();
    expect(fileExtensionBadge('Makefile')).toBeUndefined();
    expect(fileExtensionBadge('trailing.')).toBeUndefined();
  });

  it('uses the final extension of a multi-dot name', () => {
    expect(fileExtensionBadge('archive.tar.gz')).toBe('GZ');
  });
});

import { describe, expect, it } from 'vitest';

import { categoryForExtension, fileExtensionBadge } from './filePath';

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

describe('categoryForExtension', () => {
  it('maps a known extension to its colour category', () => {
    // The backend hands the preview the lowercased extension; this only maps it
    // to a dot colour, so the inputs are bare extensions, not paths.
    expect(categoryForExtension('png')).toBe('image');
    expect(categoryForExtension('rs')).toBe('code');
    expect(categoryForExtension('zip')).toBe('archive');
    expect(categoryForExtension('pdf')).toBe('document');
  });

  it('falls back to unknown for an unrecognised or missing extension', () => {
    expect(categoryForExtension('safetensors')).toBe('unknown');
    expect(categoryForExtension(undefined)).toBe('unknown');
    expect(categoryForExtension(null)).toBe('unknown');
  });
});

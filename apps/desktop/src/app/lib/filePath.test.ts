import { describe, expect, it } from 'vitest';

import {
  classifyExtension,
  fileExtensionBadge,
  findCommonParent,
  isRootOnlyPrefix,
  parentForDisplay,
  splitPath,
} from './filePath';

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

describe('classifyExtension', () => {
  it('maps known extensions to their colour category, case-insensitively', () => {
    expect(classifyExtension('photo.PNG')).toBe('image');
    expect(classifyExtension('main.rs')).toBe('code');
    expect(classifyExtension('release.zip')).toBe('archive');
    expect(classifyExtension('spec.pdf')).toBe('document');
  });

  it('falls back to unknown for unrecognised, extensionless, or dotfile names', () => {
    expect(classifyExtension('model.safetensors')).toBe('unknown');
    expect(classifyExtension('Makefile')).toBe('unknown');
    expect(classifyExtension('.env')).toBe('unknown');
    expect(classifyExtension('trailing.')).toBe('unknown');
  });

  it('classifies the basename when given a full path, ignoring dotted parents', () => {
    expect(classifyExtension('/some.dir/Makefile')).toBe('unknown');
    expect(classifyExtension('C:\\proj\\photo.png')).toBe('image');
  });
});

describe('splitPath', () => {
  it('splits a POSIX path into dimmed parent and basename', () => {
    expect(splitPath('/Users/me/proj/a.txt')).toEqual({
      dir: '/Users/me/proj/',
      base: 'a.txt',
      trailing: '',
    });
  });

  it('splits a Windows path on the backslash', () => {
    expect(splitPath('C:\\Users\\me\\report.docx')).toEqual({
      dir: 'C:\\Users\\me\\',
      base: 'report.docx',
      trailing: '',
    });
  });

  it('splits a UNC path on the trailing backslash, keeping the share prefix', () => {
    expect(splitPath('\\\\server\\share\\proj\\notes.md')).toEqual({
      dir: '\\\\server\\share\\proj\\',
      base: 'notes.md',
      trailing: '',
    });
  });

  it('treats a bare filename as having no parent', () => {
    expect(splitPath('bare.txt')).toEqual({ dir: '', base: 'bare.txt', trailing: '' });
  });

  it('collapses a trailing separator run and re-attaches a single one to the basename', () => {
    expect(splitPath('/proj/build//')).toEqual({
      dir: '/proj/',
      base: 'build',
      trailing: '/',
    });
  });
});

describe('isRootOnlyPrefix', () => {
  it('recognises lone filesystem roots across platforms', () => {
    expect(isRootOnlyPrefix('/')).toBe(true);
    expect(isRootOnlyPrefix('\\')).toBe(true);
    expect(isRootOnlyPrefix('C:\\')).toBe(true);
    expect(isRootOnlyPrefix('c:/')).toBe(true);
  });

  it('treats the bare UNC introducer as a too-noisy root', () => {
    expect(isRootOnlyPrefix('\\\\')).toBe(true);
  });

  it('does not treat a directory below the root as root-only', () => {
    expect(isRootOnlyPrefix('/tmp/')).toBe(false);
    expect(isRootOnlyPrefix('C:\\Users\\')).toBe(false);
    expect(isRootOnlyPrefix('\\\\server\\share\\')).toBe(false);
  });
});

describe('findCommonParent', () => {
  it('has no common parent for a single file', () => {
    expect(findCommonParent(['/Users/me/proj/a.txt'])).toBe('');
  });

  it('hoists the shared directory when every file lives in one folder (POSIX)', () => {
    expect(findCommonParent(['/Users/me/proj/a.txt', '/Users/me/proj/b.txt'])).toBe(
      '/Users/me/proj/',
    );
  });

  it('shrinks to the deepest shared ancestor when parents differ', () => {
    expect(findCommonParent(['/Users/me/proj/a.txt', '/Users/me/docs/b.txt'])).toBe('/Users/me/');
  });

  it('shares the deepest backslash ancestor for Windows paths', () => {
    expect(findCommonParent(['C:\\proj\\src\\a.rs', 'C:\\proj\\tests\\b.rs'])).toBe('C:\\proj\\');
  });

  it('keeps the UNC share as the common parent without POSIX mis-shortening', () => {
    expect(
      findCommonParent(['\\\\server\\share\\proj\\a.txt', '\\\\server\\share\\proj\\b.txt']),
    ).toBe('\\\\server\\share\\proj\\');
  });

  it('collapses a common parent that is only a filesystem root', () => {
    expect(findCommonParent(['/a.txt', '/b.txt'])).toBe('');
    expect(findCommonParent(['C:\\a.txt', 'C:\\b.txt'])).toBe('');
  });

  it('surfaces no header for UNC paths on different servers', () => {
    // The only shared prefix is the `\\` introducer, which is noise rather
    // than a real location — each row keeps its own server path instead.
    expect(findCommonParent(['\\\\srv1\\share\\a.txt', '\\\\srv2\\share\\b.txt'])).toBe('');
  });

  it('finds the same parent regardless of directory/file ordering', () => {
    const forward = findCommonParent(['/proj/build/', '/proj/build/file.txt']);
    const reversed = findCommonParent(['/proj/build/file.txt', '/proj/build/']);
    expect(forward).toBe('/proj/');
    expect(reversed).toBe('/proj/');
  });

  it('collapses a doubled trailing separator to the same parent as a single one', () => {
    // A non-normalised `/proj/build//` directory entry must key to `/proj/`,
    // exactly like `/proj/build/`, so its sibling does not pin the parent one
    // segment too deep and the directory row keeps rendering as `build/`.
    expect(findCommonParent(['/proj/build//', '/proj/build/file.txt'])).toBe('/proj/');
  });
});

describe('parentForDisplay', () => {
  it('drops a trailing separator so the parent reads as a place', () => {
    expect(parentForDisplay('/tmp/')).toBe('/tmp');
    expect(parentForDisplay('C:\\Users\\me\\')).toBe('C:\\Users\\me');
  });

  it('keeps a lone filesystem root intact', () => {
    expect(parentForDisplay('/')).toBe('/');
    expect(parentForDisplay('C:\\')).toBe('C:\\');
  });
});

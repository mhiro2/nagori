import { describe, expect, it } from 'vitest';

import { codeLanguageLabel, detectCodeLanguage } from './codeLanguage';

describe('codeLanguageLabel', () => {
  it('maps canonical ids to short badge labels', () => {
    expect(codeLanguageLabel('json')).toBe('JSON');
    expect(codeLanguageLabel('rust')).toBe('RS');
    expect(codeLanguageLabel('typescript')).toBe('TS');
    expect(codeLanguageLabel('python')).toBe('PY');
    expect(codeLanguageLabel('shell')).toBe('SH');
    expect(codeLanguageLabel('sql')).toBe('SQL');
    expect(codeLanguageLabel('go')).toBe('GO');
  });

  it('upper-cases an unknown id rather than dropping it', () => {
    expect(codeLanguageLabel('kotlin')).toBe('KOTLIN');
  });

  it('returns undefined for absent ids', () => {
    expect(codeLanguageLabel(undefined)).toBeUndefined();
    expect(codeLanguageLabel(null)).toBeUndefined();
    expect(codeLanguageLabel('')).toBeUndefined();
  });
});

describe('detectCodeLanguage (legacy fallback)', () => {
  it('returns canonical ids that match the badge + tokenizer vocabulary', () => {
    expect(detectCodeLanguage('{"a": 1}')).toBe('json');
    expect(detectCodeLanguage('<div>hi</div>')).toBe('html');
    expect(detectCodeLanguage('#!/bin/bash\necho hi')).toBe('shell');
    expect(detectCodeLanguage('fn main() {}')).toBe('rust');
    expect(detectCodeLanguage('const x = 1')).toBe('typescript');
    expect(detectCodeLanguage('def f():')).toBe('python');
    expect(detectCodeLanguage('SELECT * FROM t')).toBe('sql');
  });

  it('returns undefined when nothing matches', () => {
    expect(detectCodeLanguage('just some words')).toBeUndefined();
  });
});

import { describe, expect, it } from 'vitest';

import { tokenize } from './tokenize';

describe('tokenize - basics', () => {
  it('emits a single text span for a plain identifier', () => {
    const tokens = tokenize('hello');
    expect(tokens).toEqual([{ kind: 'text', text: 'hello' }]);
  });

  it('classifies known keywords distinctly from regular identifiers', () => {
    const tokens = tokenize('let foo', 'typescript');
    expect(tokens.find((t) => t.text === 'let')?.kind).toBe('kw');
    expect(tokens.find((t) => t.text === 'foo')?.kind).toBe('text');
  });

  it('highlights SQL keywords case-insensitively', () => {
    // SQL is written upper- or lower-case; the sql profile folds case so both
    // `SELECT` and `from` paint as keywords while column names stay text.
    const tokens = tokenize('SELECT id from users', 'sql');
    expect(tokens.find((t) => t.text === 'SELECT')?.kind).toBe('kw');
    expect(tokens.find((t) => t.text === 'from')?.kind).toBe('kw');
    expect(tokens.find((t) => t.text === 'users')?.kind).toBe('text');
  });

  it('does not fold case for non-SQL languages', () => {
    // `Let` (capitalised) is not a TS keyword; only the sql profile folds case.
    const tokens = tokenize('Let foo', 'typescript');
    expect(tokens.find((t) => t.text === 'Let')?.kind).toBe('text');
  });

  it('treats double-quoted strings as a single str span', () => {
    const tokens = tokenize('"hi"');
    expect(tokens).toEqual([{ kind: 'str', text: '"hi"' }]);
  });

  it('handles backtick template literals and escaped quotes', () => {
    expect(tokenize('`x`')[0]?.kind).toBe('str');
    const escaped = tokenize('"a\\"b"');
    expect(escaped[0]?.kind).toBe('str');
    expect(escaped[0]?.text).toBe('"a\\"b"');
  });

  it('captures // line comments to end-of-line', () => {
    const tokens = tokenize('// note\nlet', 'typescript');
    expect(tokens[0]).toEqual({ kind: 'comment', text: '// note' });
  });

  it('classifies numeric literals including separators', () => {
    expect(tokenize('1_000')[0]).toEqual({ kind: 'num', text: '1_000' });
    expect(tokenize('3.14')[0]).toEqual({ kind: 'num', text: '3.14' });
  });

  it('emits punct tokens for delimiters', () => {
    const tokens = tokenize('a+b');
    expect(tokens.map((t) => t.kind)).toEqual(['text', 'punct', 'text']);
  });
});

describe('tokenize - language-aware keywords', () => {
  it('does not paint `fn` as a keyword in Python source', () => {
    // `fn` lives in the Rust set but never the Python set. The old single
    // global keyword pool wrongly highlighted it as a Python variable name.
    const tokens = tokenize('fn = 1', 'python');
    expect(tokens.find((t) => t.text === 'fn')?.kind).toBe('text');
  });

  it('does not paint `def` as a keyword in Rust source', () => {
    const tokens = tokenize('let def = 1;', 'rust');
    expect(tokens.find((t) => t.text === 'def')?.kind).toBe('text');
    // Sanity: `let` is still Rust-keyword.
    expect(tokens.find((t) => t.text === 'let')?.kind).toBe('kw');
  });

  it('normalizes language aliases (ts/js/tsx → typescript)', () => {
    for (const alias of ['typescript', 'ts', 'tsx', 'js', 'jsx', 'javascript']) {
      const tokens = tokenize('const x = 1', alias);
      expect(tokens.find((t) => t.text === 'const')?.kind).toBe('kw');
    }
  });

  it('normalizes shell aliases (sh/bash/zsh → shell)', () => {
    for (const alias of ['sh', 'bash', 'zsh', 'shell']) {
      const tokens = tokenize('if true; then echo hi; fi', alias);
      expect(tokens.find((t) => t.text === 'if')?.kind).toBe('kw');
      expect(tokens.find((t) => t.text === 'then')?.kind).toBe('kw');
      expect(tokens.find((t) => t.text === 'fi')?.kind).toBe('kw');
    }
  });

  it('keeps the unlabeled default set conservative', () => {
    // Words that used to be in the global pool (e.g. `let`, `fn`, `def`) are
    // intentionally not in the default set — labelling them globally caused
    // false-positives in unlabeled prose.
    const tokens = tokenize('let fn def something');
    expect(tokens.find((t) => t.text === 'let')?.kind).toBe('text');
    expect(tokens.find((t) => t.text === 'fn')?.kind).toBe('text');
    expect(tokens.find((t) => t.text === 'def')?.kind).toBe('text');
    // `if`/`return`/`true`/`false`/`null` remain in default.
    const def = tokenize('if return true false null');
    for (const tok of def.filter((t) => t.kind !== 'text' && t.kind !== 'punct')) {
      expect(tok.kind).toBe('kw');
    }
  });
});

describe('tokenize - block comments and strings', () => {
  it('emits a single comment span for `/* ... */`', () => {
    const tokens = tokenize('/* hello\nworld */', 'rust');
    expect(tokens).toHaveLength(1);
    expect(tokens[0]).toEqual({ kind: 'comment', text: '/* hello\nworld */' });
  });

  it('emits a single comment span for `<!-- ... -->`', () => {
    const tokens = tokenize('<!-- a\nb -->');
    expect(tokens.find((t) => t.kind === 'comment')?.text).toBe('<!-- a\nb -->');
  });

  it('treats triple-quoted strings as a single str span in Python only', () => {
    const py = tokenize('"""multi\nline"""', 'python');
    expect(py).toEqual([{ kind: 'str', text: '"""multi\nline"""' }]);
    // In other languages the same source decomposes into normal single-quote
    // strings — we don't want triple-quote handling outside Python.
    const ts = tokenize('"""multi"""', 'typescript');
    expect(ts.length).toBeGreaterThan(1);
  });

  it('only treats `#` as a comment in languages that use it', () => {
    expect(tokenize('# pragma', 'python')[0]?.kind).toBe('comment');
    expect(tokenize('# header', 'shell')[0]?.kind).toBe('comment');
    // In Rust, `#[derive(...)]` is an attribute, not a comment.
    const rust = tokenize('#[derive(Debug)]', 'rust');
    expect(rust.some((t) => t.kind === 'comment')).toBe(false);
    // In TypeScript, `#` is private-class or attribute syntax.
    const ts = tokenize('#privateField', 'typescript');
    expect(ts.some((t) => t.kind === 'comment')).toBe(false);
  });
});

describe('tokenize - inline URLs', () => {
  it('classifies `https://…` as a link span', () => {
    const tokens = tokenize('See https://example.com/docs for more');
    const link = tokens.find((t) => t.kind === 'link');
    expect(link?.text).toBe('https://example.com/docs');
  });

  it('handles `http://` (no S) too', () => {
    const tokens = tokenize('http://intranet.local/x');
    expect(tokens.find((t) => t.kind === 'link')?.text).toBe('http://intranet.local/x');
  });

  it('strips trailing sentence punctuation from the link', () => {
    const tokens = tokenize('see https://example.com/page.');
    expect(tokens.find((t) => t.kind === 'link')?.text).toBe('https://example.com/page');
  });

  it('stops the URL at whitespace or closing parens', () => {
    const tokens = tokenize('(https://example.com) and more');
    expect(tokens.find((t) => t.kind === 'link')?.text).toBe('https://example.com');
  });
});

describe('tokenize - safety valve', () => {
  it('bounds span count on huge inputs', () => {
    // Realistic-ish code repeated past the 32 KiB safety valve. The first
    // 32 KiB get tokenized normally, the remainder collapses into a single
    // text span so the total span count stays well under the 15k cap from
    // the design plan.
    const line = 'const message = "hello world";\n'; // 31 bytes
    const huge = line.repeat(Math.ceil((50 * 1024) / line.length));
    expect(huge.length).toBeGreaterThan(50 * 1024);
    const tokens = tokenize(huge, 'typescript');
    expect(tokens.length).toBeLessThanOrEqual(15_000);
    // The remainder beyond 32 KiB lives in a single text span — verify that
    // by checking the last span is `text` and is reasonably large.
    const last = tokens[tokens.length - 1];
    expect(last?.kind).toBe('text');
    expect(last?.text.length ?? 0).toBeGreaterThan(8 * 1024);
  });

  it('does not engage the safety valve for normal-sized inputs', () => {
    const small = 'let x = 1;\n'.repeat(10);
    const tokens = tokenize(small, 'typescript');
    // All tokens together must reconstitute the source verbatim.
    expect(tokens.map((t) => t.text).join('')).toBe(small);
  });
});

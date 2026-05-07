// Lightweight tokenizer used purely for visual emphasis in PreviewPane. We
// deliberately avoid bringing in a full syntax highlighter (Shiki, Prism, …)
// — the goal is just to make code/JSON readable in the preview pane without
// bloating the bundle or coupling to a grammar set.
export type Span = {
  kind: 'kw' | 'str' | 'num' | 'punct' | 'comment' | 'text';
  text: string;
};

const KEYWORDS = new Set([
  'fn',
  'let',
  'mut',
  'const',
  'var',
  'if',
  'else',
  'for',
  'while',
  'return',
  'match',
  'true',
  'false',
  'null',
  'None',
  'Some',
  'Ok',
  'Err',
  'import',
  'from',
  'export',
  'default',
  'type',
  'interface',
  'class',
  'function',
  'async',
  'await',
  'new',
  'this',
  'self',
  'use',
  'pub',
  'impl',
  'struct',
  'enum',
  'trait',
  'def',
]);

// Single-pass scanner — returns tokens in order. Strings and comments win
// first so keywords inside them aren't re-coloured.
export const tokenize = (source: string): Span[] => {
  const out: Span[] = [];
  let i = 0;
  while (i < source.length) {
    const ch = source[i];

    if (ch === '"' || ch === "'" || ch === '`') {
      const quote = ch;
      let j = i + 1;
      while (j < source.length && source[j] !== quote) {
        if (source[j] === '\\') j += 2;
        else j += 1;
      }
      out.push({ kind: 'str', text: source.slice(i, Math.min(j + 1, source.length)) });
      i = j + 1;
      continue;
    }

    if (ch === '/' && source[i + 1] === '/') {
      const end = source.indexOf('\n', i);
      const stop = end === -1 ? source.length : end;
      out.push({ kind: 'comment', text: source.slice(i, stop) });
      i = stop;
      continue;
    }

    if (ch === '#' && (i === 0 || source[i - 1] === '\n')) {
      const end = source.indexOf('\n', i);
      const stop = end === -1 ? source.length : end;
      out.push({ kind: 'comment', text: source.slice(i, stop) });
      i = stop;
      continue;
    }

    if (ch !== undefined && /[0-9]/.test(ch)) {
      let j = i;
      while (j < source.length && /[0-9._]/.test(source[j] ?? '')) j += 1;
      out.push({ kind: 'num', text: source.slice(i, j) });
      i = j;
      continue;
    }

    if (ch !== undefined && /[A-Za-z_]/.test(ch)) {
      let j = i;
      while (j < source.length && /[A-Za-z0-9_]/.test(source[j] ?? '')) j += 1;
      const word = source.slice(i, j);
      out.push({ kind: KEYWORDS.has(word) ? 'kw' : 'text', text: word });
      i = j;
      continue;
    }

    if (ch !== undefined && /[{}[\](),;:.<>=+\-*/!&|?]/.test(ch)) {
      out.push({ kind: 'punct', text: ch });
      i += 1;
      continue;
    }

    out.push({ kind: 'text', text: ch ?? '' });
    i += 1;
  }
  return out;
};

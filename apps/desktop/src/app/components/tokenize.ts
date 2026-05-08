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

// charCode helpers — the per-character regex .test() calls used to dominate
// this scanner; switching to numeric comparisons is several times faster on
// the multi-KB preview bodies that PreviewPane feeds in.
const CC_0 = 48;
const CC_9 = 57;
const CC_A = 65;
const CC_Z = 90;
const CC_a = 97;
const CC_z = 122;
const CC_UNDERSCORE = 95;
const CC_DOT = 46;

const isDigit = (cc: number): boolean => cc >= CC_0 && cc <= CC_9;
const isIdentStart = (cc: number): boolean =>
  (cc >= CC_A && cc <= CC_Z) || (cc >= CC_a && cc <= CC_z) || cc === CC_UNDERSCORE;
const isIdentPart = (cc: number): boolean => isIdentStart(cc) || isDigit(cc);
const isNumPart = (cc: number): boolean => isDigit(cc) || cc === CC_DOT || cc === CC_UNDERSCORE;

const PUNCT_CHARS = new Set('{}[](),;:.<>=+-*/!&|?');

// Single-pass scanner — returns tokens in order. Strings and comments win
// first so keywords inside them aren't re-coloured.
export const tokenize = (source: string): Span[] => {
  const out: Span[] = [];
  const len = source.length;
  let i = 0;
  while (i < len) {
    const ch = source.charAt(i);
    const cc = source.charCodeAt(i);

    if (ch === '"' || ch === "'" || ch === '`') {
      const quote = ch;
      let j = i + 1;
      while (j < len && source[j] !== quote) {
        if (source[j] === '\\') j += 2;
        else j += 1;
      }
      out.push({ kind: 'str', text: source.slice(i, Math.min(j + 1, len)) });
      i = j + 1;
      continue;
    }

    if (ch === '/' && source[i + 1] === '/') {
      const end = source.indexOf('\n', i);
      const stop = end === -1 ? len : end;
      out.push({ kind: 'comment', text: source.slice(i, stop) });
      i = stop;
      continue;
    }

    if (ch === '#' && (i === 0 || source[i - 1] === '\n')) {
      const end = source.indexOf('\n', i);
      const stop = end === -1 ? len : end;
      out.push({ kind: 'comment', text: source.slice(i, stop) });
      i = stop;
      continue;
    }

    if (isDigit(cc)) {
      let j = i;
      while (j < len && isNumPart(source.charCodeAt(j))) j += 1;
      out.push({ kind: 'num', text: source.slice(i, j) });
      i = j;
      continue;
    }

    if (isIdentStart(cc)) {
      let j = i;
      while (j < len && isIdentPart(source.charCodeAt(j))) j += 1;
      const word = source.slice(i, j);
      out.push({ kind: KEYWORDS.has(word) ? 'kw' : 'text', text: word });
      i = j;
      continue;
    }

    if (PUNCT_CHARS.has(ch)) {
      out.push({ kind: 'punct', text: ch });
      i += 1;
      continue;
    }

    out.push({ kind: 'text', text: ch });
    i += 1;
  }
  return out;
};

// Lightweight tokenizer used purely for visual emphasis in PreviewPane. We
// deliberately avoid bringing in a full syntax highlighter (Shiki, Prism, …)
// — the goal is just to make code/JSON readable in the preview pane without
// bloating the bundle or coupling to a grammar set.
export type Span = {
  kind: 'kw' | 'str' | 'num' | 'punct' | 'comment' | 'text' | 'link';
  text: string;
};

// Language profiles. The default set is intentionally narrow: keywords that
// are nearly universal across the languages we care about, so unlabeled
// snippets get a tasteful amount of colour without false positives.
type LanguageId = 'default' | 'rust' | 'typescript' | 'python' | 'go' | 'shell' | 'json' | 'sql';

const KEYWORDS: Record<LanguageId, ReadonlySet<string>> = {
  default: new Set(['if', 'else', 'for', 'while', 'return', 'true', 'false', 'null']),
  rust: new Set([
    'fn',
    'let',
    'mut',
    'const',
    'static',
    'if',
    'else',
    'for',
    'while',
    'loop',
    'return',
    'match',
    'true',
    'false',
    'None',
    'Some',
    'Ok',
    'Err',
    'use',
    'pub',
    'impl',
    'struct',
    'enum',
    'trait',
    'self',
    'Self',
    'async',
    'await',
    'move',
    'ref',
    'as',
    'where',
    'break',
    'continue',
    'crate',
    'mod',
    'dyn',
    'type',
    'unsafe',
    'in',
  ]),
  typescript: new Set([
    'let',
    'const',
    'var',
    'if',
    'else',
    'for',
    'while',
    'do',
    'return',
    'true',
    'false',
    'null',
    'undefined',
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
    'extends',
    'implements',
    'instanceof',
    'typeof',
    'void',
    'try',
    'catch',
    'finally',
    'throw',
    'switch',
    'case',
    'break',
    'continue',
    'as',
    'in',
    'of',
    'yield',
    'static',
    'public',
    'private',
    'protected',
    'readonly',
  ]),
  python: new Set([
    'def',
    'if',
    'elif',
    'else',
    'for',
    'while',
    'return',
    'True',
    'False',
    'None',
    'import',
    'from',
    'class',
    'async',
    'await',
    'try',
    'except',
    'finally',
    'raise',
    'as',
    'with',
    'pass',
    'yield',
    'lambda',
    'in',
    'not',
    'and',
    'or',
    'is',
    'global',
    'nonlocal',
    'continue',
    'break',
  ]),
  go: new Set([
    'func',
    'if',
    'else',
    'for',
    'return',
    'true',
    'false',
    'nil',
    'var',
    'const',
    'type',
    'struct',
    'interface',
    'package',
    'import',
    'range',
    'chan',
    'defer',
    'go',
    'select',
    'switch',
    'case',
    'default',
    'break',
    'continue',
    'map',
  ]),
  shell: new Set([
    'if',
    'then',
    'else',
    'elif',
    'fi',
    'for',
    'while',
    'until',
    'do',
    'done',
    'case',
    'esac',
    'function',
    'return',
    'local',
    'export',
    'in',
    'break',
    'continue',
    'true',
    'false',
  ]),
  json: new Set(['true', 'false', 'null']),
  // SQL keywords are stored lower-case and looked up case-insensitively (see
  // CASE_INSENSITIVE_KW_LANGS) because SQL is written in either case.
  sql: new Set([
    'select',
    'from',
    'where',
    'insert',
    'into',
    'values',
    'update',
    'set',
    'delete',
    'create',
    'table',
    'alter',
    'drop',
    'join',
    'left',
    'right',
    'inner',
    'outer',
    'full',
    'on',
    'as',
    'and',
    'or',
    'not',
    'null',
    'is',
    'in',
    'like',
    'between',
    'group',
    'by',
    'order',
    'having',
    'limit',
    'offset',
    'distinct',
    'union',
    'all',
    'primary',
    'key',
    'foreign',
    'references',
    'index',
    'view',
    'with',
    'case',
    'when',
    'then',
    'else',
    'end',
    'exists',
    'default',
  ]),
};

// Languages whose keywords match regardless of case. Only SQL today — it is
// conventionally written upper-case (`SELECT … FROM`) as often as lower, and
// the `KEYWORDS.sql` set is stored lower-case to support the folded lookup.
const CASE_INSENSITIVE_KW_LANGS: ReadonlySet<LanguageId> = new Set<LanguageId>(['sql']);

const normalizeLanguage = (lang: string | null | undefined): LanguageId => {
  if (!lang) return 'default';
  const lower = lang.toLowerCase();
  switch (lower) {
    case 'rust':
    case 'rs':
      return 'rust';
    case 'typescript':
    case 'ts':
    case 'tsx':
    case 'javascript':
    case 'js':
    case 'jsx':
      return 'typescript';
    case 'python':
    case 'py':
      return 'python';
    case 'go':
    case 'golang':
      return 'go';
    case 'shell':
    case 'sh':
    case 'bash':
    case 'zsh':
      return 'shell';
    case 'json':
      return 'json';
    case 'sql':
      return 'sql';
    default:
      return 'default';
  }
};

// `#` is a line comment in shell/python (and our unlabeled default fallback),
// but in rust/typescript/go/json a leading `#` is meaningful syntax we'd
// rather not paint as a comment (e.g. Rust attributes `#[derive(...)]`).
const HASH_COMMENT_LANGS: ReadonlySet<LanguageId> = new Set<LanguageId>([
  'default',
  'shell',
  'python',
]);

// `"""..."""` (or `'''`) is only a string-literal block in python. Other
// languages would rather see three consecutive empty strings than a single
// span swallowing what follows.
const TRIPLE_QUOTE_LANGS: ReadonlySet<LanguageId> = new Set<LanguageId>(['python']);

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

// Past this many UTF-16 code units (≈ chars), tokenization stops and the
// remainder lands in a single `text` span. Keeps the span count (and DOM
// node count downstream) bounded for pathological pastes. Note: this is a
// character/code-unit count, not a byte count — non-ASCII bodies are
// allowed to occupy more UTF-8 bytes before the valve trips.
const SAFETY_VALVE_UNITS = 32 * 1024;

// URL terminator — `https?://` is consumed until one of these characters
// (so trailing punctuation like `).` doesn't end up inside the link).
const URL_STOP = /[\s)<>'"`]/;

const startsWith = (source: string, needle: string, at: number): boolean => {
  if (at + needle.length > source.length) return false;
  for (let k = 0; k < needle.length; k += 1) {
    if (source.charCodeAt(at + k) !== needle.charCodeAt(k)) return false;
  }
  return true;
};

const scanBlockComment = (
  source: string,
  i: number,
  open: string,
  close: string,
): { end: number } => {
  const start = i + open.length;
  const idx = source.indexOf(close, start);
  const end = idx === -1 ? source.length : idx + close.length;
  return { end };
};

// Single-pass scanner — returns tokens in order. Strings and comments win
// first so keywords inside them aren't re-coloured.
export const tokenize = (source: string, language?: string | null): Span[] => {
  const lang = normalizeLanguage(language);
  const keywords = KEYWORDS[lang];
  const caseInsensitiveKw = CASE_INSENSITIVE_KW_LANGS.has(lang);
  const hashComment = HASH_COMMENT_LANGS.has(lang);
  const tripleQuote = TRIPLE_QUOTE_LANGS.has(lang);
  const out: Span[] = [];
  const len = source.length;
  let i = 0;
  while (i < len) {
    // Safety valve: dump the rest of the source as a single text span so
    // huge inputs can't blow out the span count.
    if (i >= SAFETY_VALVE_UNITS) {
      out.push({ kind: 'text', text: source.slice(i) });
      break;
    }

    const ch = source.charAt(i);
    const cc = source.charCodeAt(i);

    // Block comment: `/* ... */` — universal across the C-family languages
    // we paint; safe to apply unconditionally because the digraph is rare in
    // shell/python source.
    if (ch === '/' && source.charCodeAt(i + 1) === 42 /* '*' */) {
      const { end } = scanBlockComment(source, i, '/*', '*/');
      out.push({ kind: 'comment', text: source.slice(i, end) });
      i = end;
      continue;
    }

    // HTML/XML comment: `<!-- ... -->`. Distinctive enough that we accept it
    // for any language without false positives.
    if (ch === '<' && startsWith(source, '<!--', i)) {
      const { end } = scanBlockComment(source, i, '<!--', '-->');
      out.push({ kind: 'comment', text: source.slice(i, end) });
      i = end;
      continue;
    }

    // Python triple-quoted strings — gated on language so other dialects
    // keep their normal single-quote behavior.
    if (tripleQuote && (ch === '"' || ch === "'")) {
      const triple = ch + ch + ch;
      if (startsWith(source, triple, i)) {
        const { end } = scanBlockComment(source, i, triple, triple);
        out.push({ kind: 'str', text: source.slice(i, end) });
        i = end;
        continue;
      }
    }

    // Inline URLs: paint `https?://…` as a link span so the reader can tell
    // it from surrounding identifiers. We deliberately leave it non-clickable
    // — opening the URL is a separate confirmed action handled elsewhere.
    if (ch === 'h' && (startsWith(source, 'http://', i) || startsWith(source, 'https://', i))) {
      let j = i;
      while (j < len && !URL_STOP.test(source.charAt(j))) j += 1;
      // Trim trailing punctuation that's commonly adjacent to a URL in prose
      // (".", ",", ";", ":") so "see https://a.example/." doesn't paint the
      // period into the link.
      while (j > i + 8) {
        const last = source.charAt(j - 1);
        if (last === '.' || last === ',' || last === ';' || last === ':') {
          j -= 1;
        } else {
          break;
        }
      }
      out.push({ kind: 'link', text: source.slice(i, j) });
      i = j;
      continue;
    }

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

    if (hashComment && ch === '#' && (i === 0 || source[i - 1] === '\n')) {
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
      const isKeyword = caseInsensitiveKw ? keywords.has(word.toLowerCase()) : keywords.has(word);
      out.push({ kind: isKeyword ? 'kw' : 'text', text: word });
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

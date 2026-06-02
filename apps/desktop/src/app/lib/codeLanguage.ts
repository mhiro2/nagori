// Code-language helpers shared by the result row. The daemon now detects a
// canonical language id (`json`, `rust`, …) at capture time and ships it on
// `SearchResultDto.language`; this module turns that id into a short badge
// label and provides a client-side fallback for legacy rows captured before
// detection landed. Both paths speak the same canonical ids the preview
// pane's tokenizer (`normalizeLanguage`) understands, so the badge label and
// the highlight profile never disagree.

// Canonical id → short uppercase badge label.
const BADGE_LABELS: Record<string, string> = {
  json: 'JSON',
  sql: 'SQL',
  shell: 'SH',
  rust: 'RS',
  typescript: 'TS',
  javascript: 'JS',
  python: 'PY',
  go: 'GO',
  yaml: 'YAML',
  toml: 'TOML',
  html: 'HTML',
  xml: 'XML',
};

// Map a canonical language id to its result-row badge label. Unknown ids fall
// back to their own upper-cased form so a future backend id still renders
// something sensible rather than vanishing.
export const codeLanguageLabel = (id: string | null | undefined): string | undefined => {
  if (!id) return undefined;
  const lower = id.toLowerCase();
  return BADGE_LABELS[lower] ?? id.toUpperCase();
};

// Ordered client-side sniff, used only when the backend `language` is absent
// (legacy code rows). Returns a canonical id so `codeLanguageLabel` and the
// preview tokenizer treat it identically to a backend-detected language. Kept
// intentionally shallower than the daemon's detector — it is a fallback, not a
// second source of truth.
const CLIENT_HEURISTICS: ReadonlyArray<{ id: string; pattern: RegExp }> = [
  { id: 'json', pattern: /^\s*[{[]/ },
  { id: 'html', pattern: /<\/?[a-z][^>]*>/i },
  { id: 'shell', pattern: /^\s*(?:#!|\$ )/ },
  { id: 'rust', pattern: /\b(?:fn|impl|struct|enum|let mut)\b/ },
  { id: 'typescript', pattern: /\b(?:const|let|interface|type|import)\b/ },
  { id: 'python', pattern: /\b(?:def|elif|import|class|self)\b/ },
  { id: 'sql', pattern: /\b(?:select|insert|update|delete|create)\b/i },
];

export const detectCodeLanguage = (preview: string): string | undefined => {
  for (const { id, pattern } of CLIENT_HEURISTICS) {
    if (pattern.test(preview)) return id;
  }
  return undefined;
};

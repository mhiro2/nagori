// Width-independent helpers for rendering captured file paths basename-first.
// The palette result row and the file-list preview both lean on this single
// module so the two surfaces classify extensions identically and never drift.
// The backend already hands the palette a home-folded `commonParentDisplay`, so
// the path-splitting helpers below serve the preview, which receives raw paths
// and has to split, group, and trim them on its own.

export type FileCategory = 'image' | 'code' | 'archive' | 'document' | 'unknown';

// Map filename extensions to a small set of categories so a row can sport a
// colour-coded dot without pulling in icon fonts.
const EXT_CATEGORY: Record<string, FileCategory> = {
  png: 'image',
  jpg: 'image',
  jpeg: 'image',
  gif: 'image',
  webp: 'image',
  svg: 'image',
  bmp: 'image',
  ico: 'image',
  heic: 'image',
  tiff: 'image',
  tif: 'image',
  avif: 'image',
  ts: 'code',
  tsx: 'code',
  js: 'code',
  jsx: 'code',
  mjs: 'code',
  cjs: 'code',
  rs: 'code',
  go: 'code',
  py: 'code',
  rb: 'code',
  java: 'code',
  kt: 'code',
  swift: 'code',
  c: 'code',
  cpp: 'code',
  cc: 'code',
  h: 'code',
  hpp: 'code',
  cs: 'code',
  php: 'code',
  sh: 'code',
  bash: 'code',
  zsh: 'code',
  sql: 'code',
  json: 'code',
  xml: 'code',
  yaml: 'code',
  yml: 'code',
  toml: 'code',
  html: 'code',
  htm: 'code',
  css: 'code',
  scss: 'code',
  sass: 'code',
  less: 'code',
  vue: 'code',
  svelte: 'code',
  md: 'code',
  rst: 'code',
  zip: 'archive',
  tar: 'archive',
  gz: 'archive',
  tgz: 'archive',
  bz2: 'archive',
  xz: 'archive',
  '7z': 'archive',
  rar: 'archive',
  dmg: 'archive',
  iso: 'archive',
  pdf: 'document',
  doc: 'document',
  docx: 'document',
  xls: 'document',
  xlsx: 'document',
  ppt: 'document',
  pptx: 'document',
  txt: 'document',
  rtf: 'document',
  odt: 'document',
  ods: 'document',
  odp: 'document',
  csv: 'document',
  tsv: 'document',
};

// Curated allowlist of known, short extensions surfaced as an uppercase badge on
// single-file result rows (`PPTX`, `PDF`, `XLSX`, …). Anything outside it — an
// unfamiliar or arbitrarily long extension, a leading-dot file, an extensionless
// name — falls back to a generic badge so the fixed-width badge column never
// stretches. The badge is a basename-derived hint, independent of any file-kind
// detection: a directory literally named `report.pdf` would badge as PDF, which
// is rare enough to accept until the backend carries a kind.
const BADGE_EXTENSIONS = new Set<string>([
  // Documents
  'pdf',
  'doc',
  'docx',
  'xls',
  'xlsx',
  'ppt',
  'pptx',
  'txt',
  'rtf',
  'csv',
  'tsv',
  'md',
  'key',
  'odt',
  'ods',
  'odp',
  'epub',
  // Images
  'png',
  'jpg',
  'jpeg',
  'gif',
  'webp',
  'svg',
  'bmp',
  'ico',
  'heic',
  'tiff',
  'tif',
  'avif',
  // Archives
  'zip',
  'gz',
  'tgz',
  'tar',
  'bz2',
  'xz',
  '7z',
  'rar',
  'dmg',
  'iso',
  // Audio / video
  'mp3',
  'wav',
  'flac',
  'aac',
  'm4a',
  'ogg',
  'mp4',
  'mov',
  'avi',
  'mkv',
  'webm',
  // Code / data
  'rs',
  'ts',
  'tsx',
  'js',
  'jsx',
  'mjs',
  'cjs',
  'py',
  'go',
  'rb',
  'java',
  'kt',
  'swift',
  'c',
  'cpp',
  'cc',
  'h',
  'hpp',
  'cs',
  'php',
  'sh',
  'sql',
  'json',
  'xml',
  'yaml',
  'yml',
  'toml',
  'html',
  'htm',
  'css',
  'scss',
  'vue',
]);

// Upper bound so an allowlist edit can never reintroduce a column-breaking
// badge; every entry above already fits within it.
const MAX_BADGE_LENGTH = 4;

// The lowercased extension of a path or bare filename, or `undefined` when there
// is none worth surfacing: an extensionless name (`Makefile`), a leading-dot
// file (`.env`), a trailing dot (`report.`), or a dot that lives inside a parent
// directory (`/some.dir/Makefile`). It accepts a full path or a basename alike —
// a basename simply has no separator, so the parent-dir guard is a no-op for it.
// This single extraction is what keeps the palette (basenames) and the preview
// (full paths) classifying extensions the same way.
const extensionOf = (pathOrName: string): string | undefined => {
  const lastSlash = Math.max(pathOrName.lastIndexOf('/'), pathOrName.lastIndexOf('\\'));
  const dot = pathOrName.lastIndexOf('.');
  // A dot at or before the first basename character belongs to a parent
  // directory (`/some.dir/x`) or a leading-dot file (`.env`), not an extension.
  if (dot <= lastSlash + 1) return undefined;
  if (dot === pathOrName.length - 1) return undefined;
  return pathOrName.slice(dot + 1).toLowerCase();
};

// Uppercase extension badge for a basename, or `undefined` to use a generic
// glyph. Returns `undefined` for leading-dot files (`.env`), extensionless
// names, and extensions outside the curated allowlist.
export const fileExtensionBadge = (name: string): string | undefined => {
  const ext = extensionOf(name);
  if (ext === undefined || ext.length > MAX_BADGE_LENGTH) return undefined;
  return BADGE_EXTENSIONS.has(ext) ? ext.toUpperCase() : undefined;
};

// Colour category for a path or basename's extension, or `unknown` when the
// extension is absent or unrecognised. Deliberately knows nothing about
// directories: a trailing separator is not a reliable directory signal in
// captured OS paths, so any folder treatment is left to the caller.
export const classifyExtension = (pathOrName: string): FileCategory => {
  const ext = extensionOf(pathOrName);
  return ext !== undefined ? (EXT_CATEGORY[ext] ?? 'unknown') : 'unknown';
};

// Split on the last `/` or `\` so Windows-style file lists also light up the
// basename emphasis. The dir portion keeps its trailing separator so the visual
// order is "<dim>parent/</dim><strong>basename</strong>". A path ending in one
// or more separators represents a directory; we strip the whole trailing run
// before splitting and return a single representative separator in `trailing` so
// the caller can re-attach it to the basename (`foo/` rather than `foo`).
// Collapsing the run keeps a non-normalised `…/dir//` from yielding an empty
// basename.
export const splitPath = (path: string): { dir: string; base: string; trailing: string } => {
  const body = path.replace(/[/\\]+$/, '');
  const isDir = body.length < path.length;
  // The first stripped char stands in for the (possibly repeated) trailing run.
  const trailing = isDir ? path.charAt(body.length) : '';
  const lastSlash = Math.max(body.lastIndexOf('/'), body.lastIndexOf('\\'));
  if (lastSlash < 0) return { dir: '', base: body, trailing };
  return {
    dir: body.slice(0, lastSlash + 1),
    base: body.slice(lastSlash + 1),
    trailing,
  };
};

// Index just past the last separator that delimits parent-from-basename in `s`,
// or 0 if none. A trailing separator run (e.g. `/proj/build/` or a non-normalised
// `/proj/build//`) is treated as part of the directory's own name rather than as
// the delimiter, so the parent extracted from either is `/proj/` and the entry
// can render under that header without becoming an empty row. The whole run is
// stripped — matching `splitPath` — so a doubled trailing separator does not
// pin the parent one segment too deep.
const dirEndOf = (s: string): number => {
  const body = s.replace(/[/\\]+$/, '');
  const last = Math.max(body.lastIndexOf('/'), body.lastIndexOf('\\'));
  return last < 0 ? 0 : last + 1;
};

// A lone filesystem root is too noisy to surface as a common-parent header —
// every row would still need its own absolute prefix to be readable, so callers
// collapse it to `''`. Covers a POSIX root (`/`), a bare separator (`\`), a
// Windows drive root (`C:\`), and the bare UNC introducer (`\\`) that two paths
// on different servers shrink down to.
export const isRootOnlyPrefix = (s: string): boolean =>
  s === '/' || s === '\\' || s === '\\\\' || /^[A-Za-z]:[\\/]$/.test(s);

// Longest common directory prefix shared by every path in the list. We compare
// each entry's *parent-directory candidate* (`dirEndOf`-trimmed slice) rather
// than the raw path so the algorithm is order-independent — otherwise a
// directory entry appearing later than its sibling file would pin the prefix at
// the directory itself and collapse that row to empty. Operates on character
// ranges between separators so we never split inside a path segment. A prefix
// that shrinks to a lone filesystem root collapses to `''`.
export const findCommonParent = (input: readonly string[]): string => {
  if (input.length < 2) return '';
  const parents = input.map((p) => p.slice(0, dirEndOf(p)));
  let prefix = parents[0]!;
  for (let i = 1; i < parents.length && prefix.length > 0; i += 1) {
    const parent = parents[i]!;
    while (prefix.length > 0 && !parent.startsWith(prefix)) {
      // Shrink to the next-shorter directory by dropping the trailing
      // separator and re-finding the previous one.
      const trimmed = prefix.slice(0, -1);
      prefix = trimmed.slice(0, dirEndOf(trimmed));
    }
  }
  if (isRootOnlyPrefix(prefix)) return '';
  return prefix;
};

// Parent directory formatted as a location: drop the trailing separator
// (`/tmp/` → `/tmp`) so it reads as a place, but keep a filesystem root intact —
// `/`, `\`, and `C:\` are meaningful as-is and `C:\` must not collapse to the
// drive-relative `C:`.
export const parentForDisplay = (dir: string): string =>
  isRootOnlyPrefix(dir) ? dir : dir.replace(/[/\\]+$/, '');

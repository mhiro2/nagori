// Extension classification for captured file rows. The backend now hands both
// the palette (`commonParentDisplay` + representative names) and the preview
// (`FileEntry[]`) their paths already split and home-folded, so the only path
// work left on the renderer is reading a filename's extension: the palette turns
// it into an uppercase badge and the preview turns it into a colour-dot
// category. Both lean on the single `extensionOf` primitive here so the two
// surfaces never disagree about what counts as an extension.

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

// Colour category for an already-extracted extension (the backend hands the
// preview the lowercased `extension` directly), or `unknown` when the extension
// is absent or unrecognised. Deliberately knows nothing about directories: the
// preview squares the dot off for a folder by reading the row's trailing
// separator, so any folder treatment is left to the caller.
export const categoryForExtension = (ext: string | null | undefined): FileCategory =>
  ext != null ? (EXT_CATEGORY[ext] ?? 'unknown') : 'unknown';

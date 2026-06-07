// Curated allowlist of known, short file extensions surfaced as an uppercase
// badge on single-file result rows (`PPTX`, `PDF`, `XLSX`, …). Anything outside
// it — an unfamiliar or arbitrarily long extension, a leading-dot file, an
// extensionless name — falls back to a generic badge so the fixed-width badge
// column never stretches. The badge is a basename-derived hint, independent of
// any file-kind detection: a directory literally named `report.pdf` would badge
// as PDF, which is rare enough to accept until the backend carries a kind.
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

// Uppercase extension badge for a basename, or `undefined` to use a generic
// glyph. Returns `undefined` for leading-dot files (`.env`), extensionless
// names, and extensions outside the curated allowlist.
export const fileExtensionBadge = (name: string): string | undefined => {
  const dot = name.lastIndexOf('.');
  if (dot <= 0 || dot === name.length - 1) return undefined;
  const ext = name.slice(dot + 1).toLowerCase();
  if (ext.length > MAX_BADGE_LENGTH) return undefined;
  return BADGE_EXTENSIONS.has(ext) ? ext.toUpperCase() : undefined;
};

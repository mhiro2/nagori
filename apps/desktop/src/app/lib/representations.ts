// User-facing summary of the *extra* formats a clip kept beyond its main
// content. A copy often carries several representations (e.g. a file list that
// also brought a PNG render and a plain-text fallback); rather than dumping the
// raw MIME list ("PNG + Plain") onto every row, we surface only the additional
// data — folded into coarse, translatable categories — and only in the preview
// pane's Details. The primary representation's own category is excluded so we
// never echo the kind the row already shows.

import type { RepresentationSummary } from './types';

// Coarse, user-facing buckets. The localised label for each lives in the i18n
// `preview.clipboardCategory` map so the renderer never shows a raw MIME type.
export type ClipboardCategory = 'image' | 'text' | 'files';

const categoryOf = (mime: string): ClipboardCategory => {
  if (mime.startsWith('image/')) return 'image';
  if (mime === 'text/uri-list') return 'files';
  // Everything else allowlisted is text-shaped (plain / html / rtf).
  return 'text';
};

// The additional-data categories present in `summary`, excluding the primary
// representation's own category and de-duplicated in clipboard order. Empty when
// the entry carries nothing beyond its primary kind — callers then hide the row.
export const additionalClipboardCategories = (
  summary: readonly RepresentationSummary[] | undefined,
): ClipboardCategory[] => {
  if (!summary || summary.length === 0) return [];
  const primary = summary.find((rep) => rep.role === 'primary');
  const primaryCategory = primary ? categoryOf(primary.mimeType) : undefined;
  const categories: ClipboardCategory[] = [];
  for (const rep of summary) {
    const category = categoryOf(rep.mimeType);
    if (category === primaryCategory) continue;
    if (!categories.includes(category)) categories.push(category);
  }
  return categories;
};

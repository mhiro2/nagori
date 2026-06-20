// User-facing summary of the *extra* formats a clip kept beyond its main
// content. A copy often carries several representations (e.g. a file list that
// also brought a PNG render and a plain-text fallback); rather than dumping the
// raw MIME list ("PNG + Plain") onto every row, we surface only the additional
// data — folded into coarse, translatable categories — in the preview pane's
// resting footer. The primary representation's own category is excluded so we
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

// Image MIME types the daemon's thumbnail pipeline can actually decode —
// mirrors `nagori_core::SUPPORTED_IMAGE_MIMES`. A clip can carry an image in a
// format the pipeline rejects (SVG can host script; HEIC is unsupported), so
// gating on this set rather than a bare `image/` prefix avoids asking for a
// `/thumb/<id>` the generator can never produce (the storage-side lookup
// filters to this same set).
const THUMBNAILABLE_IMAGE_MIMES = new Set([
  'image/png',
  'image/jpeg',
  'image/gif',
  'image/webp',
  'image/tiff',
]);

// Whether the clip kept a thumbnailable image alongside (not as) its primary
// content — e.g. a file copy that also carried an `image/png` render of the
// copied object. The file-list preview uses this to show a small supplementary
// thumbnail. The primary representation is excluded on purpose: an image-kind
// entry already renders its own image, so this only reports a *secondary*
// image.
export const hasAccompanyingImage = (
  summary: readonly RepresentationSummary[] | undefined,
): boolean =>
  summary?.some((rep) => rep.role !== 'primary' && THUMBNAILABLE_IMAGE_MIMES.has(rep.mimeType)) ??
  false;

// MIME types Nagori can re-publish to the clipboard — mirrors the daemon's
// per-adapter publishable allowlist (see ARCHITECTURE.md `ClipboardWriter`).
// Kept here so the desktop can decide *synchronously*, from the row's
// representation summary, whether an entry offers a real "paste as <format>"
// choice without an IPC round-trip.
const PUBLISHABLE_MIMES = new Set([
  'text/plain',
  'text/html',
  'application/rtf',
  'image/png',
  'image/tiff',
  'image/jpeg',
  'image/gif',
  'image/webp',
  'text/uri-list',
]);

// Whether the entry can be pasted in more than one distinct format — i.e. the
// "paste as" picker would offer a genuine choice (≥2 publishable MIMEs). The
// right-click menu uses this to show its *Paste as…* row only when it does
// something the plain *Paste* row doesn't, mirroring the ⇧⌘⏎ chord's own
// ≥2-formats gate. `list_paste_options` remains the authority at paste time;
// this is the cheap, summary-derived approximation for the menu affordance.
export const offersPasteFormatChoice = (
  summary: readonly RepresentationSummary[] | undefined,
): boolean => {
  if (!summary) return false;
  const mimes = new Set<string>();
  for (const rep of summary) {
    if (PUBLISHABLE_MIMES.has(rep.mimeType)) mimes.add(rep.mimeType);
  }
  return mimes.size >= 2;
};

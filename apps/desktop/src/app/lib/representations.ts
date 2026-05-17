// Short labels for the per-MIME chips that show "this clip also kept HTML,
// RTF, etc." in the palette row and the preview pane. The publisher already
// collapses primary + plain_fallback pairs that share a MIME, but we dedupe
// labels too so e.g. a primary text/plain + alternative text/plain renders
// once rather than "Plain, Plain".

import type { RepresentationSummary } from './types';

const REP_LABEL_BY_MIME: Record<string, string> = {
  'text/plain': 'Plain',
  'text/html': 'HTML',
  'application/rtf': 'RTF',
  'text/uri-list': 'Files',
  'image/png': 'PNG',
  'image/jpeg': 'JPEG',
  'image/gif': 'GIF',
  'image/webp': 'WebP',
  'image/tiff': 'TIFF',
};

export const representationLabel = (mime: string): string => {
  const known = REP_LABEL_BY_MIME[mime];
  if (known !== undefined) return known;
  if (mime.startsWith('image/')) return 'IMG';
  return mime;
};

// Returns the de-duplicated label list, preserving the input order. Empty
// when the entry only has its primary representation — callers decide how
// to render the "single format" case (usually: don't show the row at all).
export const dedupedRepresentationLabels = (
  summary: readonly RepresentationSummary[] | undefined,
): string[] => {
  if (!summary || summary.length === 0) return [];
  const seen = new Set<string>();
  const labels: string[] = [];
  for (const rep of summary) {
    const label = representationLabel(rep.mimeType);
    if (seen.has(label)) continue;
    seen.add(label);
    labels.push(label);
  }
  return labels;
};

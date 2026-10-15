// Query-match highlighting shared by the result list (ResultItem) and the
// preview pane (PreviewBodyText). This is deliberately *not* the syntax
// tokenizer in `components/tokenize.ts`: that one colours code by grammar,
// this one marks where the user's search query lands in the displayed text.
//
// Matching is intentionally simple — a case-insensitive raw substring scan,
// one independent pass per whitespace-separated query term. We do NOT apply
// the backend's NFKC normalisation: mapping a normalised hit position back
// onto the raw preview is fiddly and not worth it for a visual cue. The
// consequence (a half-width `ABC` query does not mark a full-width `ＡＢＣ`
// body, and vice-versa) is an accepted limitation; FTS / semantic hits that
// have no literal substring in the visible text simply produce no marks and
// rely on the rank-reason chip to explain why the row matched.
export type HighlightSegment = {
  text: string;
  match: boolean;
  // Ordinal of the match range a marked segment belongs to, so the preview
  // can step between matches. Absent on unmatched segments.
  matchIndex?: number;
};

export type MatchRange = readonly [start: number, end: number];

// Past this many UTF-16 code units we stop scanning for matches and emit the
// remainder as one unmatched segment. Row previews are already truncated
// (~120 chars), but the preview pane feeds in bodies up to 128 KiB, so the
// cap keeps the scan — and the rendered segment count — bounded. Mirrors the
// spirit of tokenize.ts's SAFETY_VALVE_UNITS.
const MAX_HIGHLIGHT_CHARS = 32 * 1024;

// Upper bound on collected match ranges. A single-character query against a
// long body could otherwise mark thousands of spots and explode the DOM node
// count; once we hit the cap the rest of the body stays unmarked.
const MAX_MATCH_RANGES = 500;

/**
 * Merged, ordered `[start, end)` ranges where any whitespace-separated term of
 * `query` occurs in `text` (case-insensitive raw substring). Empty when the
 * query is blank or nothing matches. Only the first `MAX_HIGHLIGHT_CHARS`
 * code units are scanned and at most `MAX_MATCH_RANGES` hits are collected.
 */
export const matchRanges = (text: string, query: string | undefined): MatchRange[] => {
  if (text.length === 0) return [];
  const trimmed = query?.trim().toLowerCase() ?? '';
  if (trimmed === '') return [];
  // Dedupe so a repeated term ("foo foo") doesn't do redundant scans; the
  // longest-first order makes overlapping terms merge into the widest span
  // rather than nest.
  const terms = [...new Set(trimmed.split(/\s+/).filter((term) => term.length > 0))].toSorted(
    (a, b) => b.length - a.length,
  );
  if (terms.length === 0) return [];

  const scanLimit = Math.min(text.length, MAX_HIGHLIGHT_CHARS);
  const original = text.slice(0, scanLimit);
  const haystack = original.toLowerCase();
  // `toLowerCase()` is length-preserving for ASCII / CJK / most scripts, but a
  // few code points (e.g. `İ`) fold to a different length, which would
  // misalign the indices we slice back out of `text`. Rather than risk a
  // garbled or surrogate-split render, skip highlighting entirely in that rare
  // case.
  if (haystack.length !== original.length) return [];

  const ranges: Array<[number, number]> = [];
  outer: for (const term of terms) {
    let from = 0;
    for (;;) {
      const idx = haystack.indexOf(term, from);
      if (idx === -1) break;
      ranges.push([idx, idx + term.length]);
      from = idx + term.length;
      if (ranges.length >= MAX_MATCH_RANGES) break outer;
    }
  }

  // Merge overlapping / touching ranges so we never emit nested or adjacent
  // marks.
  const sorted = ranges.toSorted((a, b) => a[0] - b[0] || a[1] - b[1]);
  const merged: Array<[number, number]> = [];
  for (const [start, end] of sorted) {
    const last = merged.at(-1);
    if (last && start <= last[1]) {
      if (end > last[1]) last[1] = end;
    } else {
      merged.push([start, end]);
    }
  }
  return merged;
};

/**
 * Split `text` into alternating matched / unmatched segments for the given
 * `query`. Returns a single unmatched segment when the query is empty or no
 * term occurs in the text. The concatenation of every segment's `text` always
 * reconstitutes the original `text` verbatim, so callers can render each
 * segment with plain text interpolation (never `@html`) and stay XSS-safe.
 */
export const highlightQuery = (text: string, query: string | undefined): HighlightSegment[] => {
  if (text.length === 0) return [];
  const ranges = matchRanges(text, query);
  if (ranges.length === 0) return [{ text, match: false }];
  // Build segments over the FULL text so anything past the scan limit is kept
  // verbatim as a trailing unmatched segment.
  const segments: HighlightSegment[] = [];
  let cursor = 0;
  ranges.forEach(([start, end], matchIndex) => {
    if (start > cursor) segments.push({ text: text.slice(cursor, start), match: false });
    segments.push({ text: text.slice(start, end), match: true, matchIndex });
    cursor = end;
  });
  if (cursor < text.length) segments.push({ text: text.slice(cursor), match: false });
  return segments;
};

/**
 * Overlay match `ranges` onto a run of styled spans whose texts concatenate
 * to the matched text (the syntax tokenizer's output), splitting a span
 * wherever a match starts or ends. Each piece keeps its span's fields, so a
 * code body keeps its grammar colouring inside and around the marks.
 */
export const markSpans = <S extends { text: string }>(
  spans: readonly S[],
  ranges: readonly MatchRange[],
): Array<S & { matchIndex?: number }> => {
  if (ranges.length === 0) return [...spans];
  const out: Array<S & { matchIndex?: number }> = [];
  let offset = 0;
  let rangeIdx = 0;
  for (const span of spans) {
    const spanEnd = offset + span.text.length;
    let cursor = offset;
    while (cursor < spanEnd) {
      while (rangeIdx < ranges.length && ranges[rangeIdx]![1] <= cursor) rangeIdx += 1;
      const range = ranges[rangeIdx];
      if (range === undefined || range[0] >= spanEnd) {
        out.push({ ...span, text: span.text.slice(cursor - offset) });
        cursor = spanEnd;
      } else if (range[0] > cursor) {
        out.push({ ...span, text: span.text.slice(cursor - offset, range[0] - offset) });
        cursor = range[0];
      } else {
        const end = Math.min(range[1], spanEnd);
        out.push({
          ...span,
          text: span.text.slice(cursor - offset, end - offset),
          matchIndex: rangeIdx,
        });
        cursor = end;
      }
    }
    offset = spanEnd;
  }
  return out;
};

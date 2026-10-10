import { describe, expect, it } from 'vitest';

import { highlightQuery, type HighlightSegment, markSpans, matchRanges } from './highlightQuery';

// Convenience: the matched substrings, in order.
const matches = (segments: HighlightSegment[]): string[] =>
  segments.filter((s) => s.match).map((s) => s.text);

// Invariant every result must satisfy: segments reconstitute the source.
const rebuilt = (segments: HighlightSegment[]): string => segments.map((s) => s.text).join('');

describe('highlightQuery', () => {
  it('returns an empty array for empty text', () => {
    expect(highlightQuery('', 'foo')).toEqual([]);
  });

  it('returns a single unmatched segment when the query is empty', () => {
    expect(highlightQuery('hello world', '')).toEqual([{ text: 'hello world', match: false }]);
    expect(highlightQuery('hello world', undefined)).toEqual([
      { text: 'hello world', match: false },
    ]);
    expect(highlightQuery('hello world', '   ')).toEqual([{ text: 'hello world', match: false }]);
  });

  it('marks a single case-insensitive substring hit', () => {
    const segments = highlightQuery('The Needle here', 'needle');
    expect(matches(segments)).toEqual(['Needle']);
    expect(rebuilt(segments)).toBe('The Needle here');
    // The mark preserves the original casing of the body, not the query.
    expect(segments).toEqual([
      { text: 'The ', match: false },
      { text: 'Needle', match: true, matchIndex: 0 },
      { text: ' here', match: false },
    ]);
  });

  it('marks every occurrence of a term', () => {
    const segments = highlightQuery('aXaXa', 'a');
    expect(matches(segments)).toEqual(['a', 'a', 'a']);
    expect(rebuilt(segments)).toBe('aXaXa');
  });

  it('highlights each whitespace-separated term independently', () => {
    const segments = highlightQuery('foo and bar', 'foo bar');
    expect(matches(segments)).toEqual(['foo', 'bar']);
    expect(rebuilt(segments)).toBe('foo and bar');
  });

  it('merges overlapping terms into the widest span (no nested marks)', () => {
    // `foo` and `foobar` both hit; the longer one wins and the shorter is
    // absorbed rather than producing a nested or duplicated mark.
    const segments = highlightQuery('see foobar end', 'foo foobar');
    expect(matches(segments)).toEqual(['foobar']);
    expect(rebuilt(segments)).toBe('see foobar end');
  });

  it('merges adjacent matches without splitting them', () => {
    const segments = highlightQuery('abcabc', 'abc');
    // Two adjacent `abc` runs merge into one contiguous mark.
    expect(matches(segments)).toEqual(['abcabc']);
    expect(rebuilt(segments)).toBe('abcabc');
  });

  it('highlights CJK substrings (no tokenisation needed)', () => {
    const segments = highlightQuery('東京都の天気', '東京');
    expect(matches(segments)).toEqual(['東京']);
    expect(rebuilt(segments)).toBe('東京都の天気');
  });

  it('returns no marks when no term occurs (FTS/semantic hits)', () => {
    const segments = highlightQuery('totally unrelated body', 'needle');
    expect(matches(segments)).toEqual([]);
    expect(segments).toEqual([{ text: 'totally unrelated body', match: false }]);
  });

  it('keeps half-width vs full-width distinct (NFKC is intentionally skipped)', () => {
    // A half-width query does not mark a full-width body — documented limitation.
    expect(matches(highlightQuery('ＡＢＣ', 'abc'))).toEqual([]);
    // ...but a full-width query against a full-width body still works.
    expect(matches(highlightQuery('ＡＢＣ', 'ＡＢＣ'))).toEqual(['ＡＢＣ']);
  });

  it('bounds the match count on a pathological single-char query', () => {
    const body = 'a'.repeat(5000);
    const segments = highlightQuery(body, 'a');
    // Capped well under the input size, and the body is still reconstructed.
    expect(matches(segments).length).toBeLessThanOrEqual(500);
    expect(rebuilt(segments)).toBe(body);
  });

  it('keeps the tail past the scan cap verbatim and unmarked', () => {
    // A match before the cap is highlighted; everything past the cap is one
    // trailing unmatched segment, and the whole body still round-trips.
    const head = `needle${'x'.repeat(40 * 1024)}`;
    const segments = highlightQuery(head, 'needle');
    expect(matches(segments)).toEqual(['needle']);
    expect(rebuilt(segments)).toBe(head);
    expect(segments.at(-1)?.match).toBe(false);
  });
});

describe('matchRanges / markSpans', () => {
  it('numbers each merged match range in order', () => {
    const segments = highlightQuery('foo bar foo', 'foo');
    expect(segments.filter((s) => s.match).map((s) => s.matchIndex)).toEqual([0, 1]);
    expect(matchRanges('foo bar foo', 'foo')).toEqual([
      [0, 3],
      [8, 11],
    ]);
  });

  it('splits styled spans at match edges and keeps their styling', () => {
    const spans = [
      { kind: 'kw', text: 'let' },
      { kind: 'text', text: ' ' },
      { kind: 'text', text: 'order_total' },
      { kind: 'punct', text: ';' },
    ];
    // One match sits inside a span, the other crosses into the punctuation.
    const marked = markSpans(spans, matchRanges('let order_total;', 'ord al;'));
    expect(marked.map((s) => s.text).join('')).toBe('let order_total;');
    expect(marked.filter((s) => s.matchIndex !== undefined)).toEqual([
      { kind: 'text', text: 'ord', matchIndex: 0 },
      { kind: 'text', text: 'al', matchIndex: 1 },
      { kind: 'punct', text: ';', matchIndex: 1 },
    ]);
  });

  it('returns the spans untouched without matches', () => {
    const spans = [{ kind: 'text', text: 'abc' }];
    expect(markSpans(spans, [])).toEqual(spans);
  });
});

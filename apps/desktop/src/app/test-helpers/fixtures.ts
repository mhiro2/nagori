import type { EntryPreviewDto, SearchResultDto } from '../lib/types';

// Shared builders for the backend DTOs most specs construct. Defaults are a
// plain public text entry; tests override the fields they assert on (many
// keep a local one-liner wrapper to bake in their own preview text).

export const sampleSearchResult = (overrides: Partial<SearchResultDto> = {}): SearchResultDto => ({
  id: 'entry-id',
  kind: 'text',
  preview: 'preview body',
  score: 0,
  createdAt: '2026-05-05T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: ['Recent'],
  representationSummary: [],
  ...overrides,
});

// `metadata` merges field-wise so callers can adjust a single counter
// without restating the whole block.
export const sampleEntryPreview = (
  overrides: Partial<Omit<EntryPreviewDto, 'metadata'>> & {
    metadata?: Partial<EntryPreviewDto['metadata']>;
  } = {},
): EntryPreviewDto => ({
  id: 'entry-id',
  kind: 'text',
  title: 'Title',
  previewText: 'preview body',
  body: { type: 'text', text: 'preview body' },
  ...overrides,
  metadata: {
    byteCount: 12,
    charCount: 12,
    lineCount: 1,
    truncated: false,
    sensitive: false,
    fullContentAvailable: true,
    ...overrides.metadata,
  },
});

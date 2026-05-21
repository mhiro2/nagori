// Preview hydration for the currently-selected entry. Kept in its own
// store so the preview pane can re-render independently of search-result
// churn, and so the in-flight ticket logic stays scoped to preview fetches.

import { getEntryPreview, getEntryPreviewFull } from '../lib/commands';
import { describeError } from '../lib/errors';
import { isTauri } from '../lib/tauri';
import type { EntryPreviewDto } from '../lib/types';

type PreviewState = {
  entryId: string | undefined;
  // Remembering the query lets us re-hydrate when only the search string
  // changes: `elidedContainsMatch` is computed per-query on the backend, so
  // sticking with the previous result would leave a stale "match in middle"
  // hint (or miss a real one) after the user keeps typing.
  query: string | undefined;
  preview: EntryPreviewDto | undefined;
  loading: boolean;
  errorMessage: string | undefined;
  // True while the expanded body is being fetched via `getEntryPreviewFull`.
  // Separate from `loading` so the caller can render a partial overlay
  // (e.g. spinner on the expand button) without dropping the existing
  // standard-cap body.
  expandedLoading: boolean;
  expandedErrorMessage: string | undefined;
};

export const previewState = $state<PreviewState>({
  entryId: undefined,
  query: undefined,
  preview: undefined,
  loading: false,
  errorMessage: undefined,
  expandedLoading: false,
  expandedErrorMessage: undefined,
});

let previewInflight = 0;
let expandedInflight = 0;

export const hydratePreview = async (
  entryId: string | undefined,
  query?: string,
): Promise<void> => {
  const sameEntry = previewState.entryId === entryId;
  const sameQuery = previewState.query === query;
  if (
    sameEntry &&
    sameQuery &&
    (previewState.preview || previewState.loading || previewState.errorMessage)
  ) {
    return;
  }
  previewState.entryId = entryId;
  previewState.query = query;
  // Keep the existing preview body visible while we refetch on a query-only
  // change so the user doesn't get a flash of "loading…". A switch to a new
  // entry drops the old body because the IDs no longer agree.
  if (!sameEntry) {
    previewState.preview = undefined;
  }
  previewState.errorMessage = undefined;
  previewState.expandedErrorMessage = undefined;
  previewState.expandedLoading = false;
  if (!entryId || !isTauri()) {
    previewState.loading = false;
    return;
  }
  const ticket = ++previewInflight;
  previewState.loading = true;
  try {
    const preview = await getEntryPreview(entryId, query);
    if (
      ticket !== previewInflight ||
      previewState.entryId !== entryId ||
      previewState.query !== query
    )
      return;
    previewState.preview = preview;
  } catch (err) {
    if (
      ticket !== previewInflight ||
      previewState.entryId !== entryId ||
      previewState.query !== query
    )
      return;
    previewState.errorMessage = describeError(err);
  } finally {
    if (
      ticket === previewInflight &&
      previewState.entryId === entryId &&
      previewState.query === query
    ) {
      previewState.loading = false;
    }
  }
};

/// Replace the current standard-cap preview with the expanded 1 MiB body.
/// No-op when the entry id no longer matches the active selection, when
/// the backend is unavailable, or when the body was not truncated in the
/// first place — calling here on an already-full body would round-trip
/// for nothing. Errors are routed to `expandedErrorMessage` so the
/// standard preview stays visible.
export const expandPreview = async (entryId: string): Promise<void> => {
  if (!isTauri() || previewState.entryId !== entryId) return;
  if (previewState.preview && !previewState.preview.metadata.truncated) return;
  const ticket = ++expandedInflight;
  previewState.expandedLoading = true;
  previewState.expandedErrorMessage = undefined;
  try {
    const full = await getEntryPreviewFull(entryId);
    if (ticket !== expandedInflight || previewState.entryId !== entryId) return;
    previewState.preview = full;
  } catch (err) {
    if (ticket !== expandedInflight || previewState.entryId !== entryId) return;
    previewState.expandedErrorMessage = describeError(err);
  } finally {
    if (ticket === expandedInflight && previewState.entryId === entryId) {
      previewState.expandedLoading = false;
    }
  }
};

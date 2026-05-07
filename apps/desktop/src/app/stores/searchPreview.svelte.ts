// Preview hydration for the currently-selected entry. Kept in its own
// store so the preview pane can re-render independently of search-result
// churn, and so the in-flight ticket logic stays scoped to preview fetches.

import { getEntryPreview } from '../lib/commands';
import { describeError } from '../lib/errors';
import { isTauri } from '../lib/tauri';
import type { EntryPreviewDto } from '../lib/types';

type PreviewState = {
  entryId: string | undefined;
  preview: EntryPreviewDto | undefined;
  loading: boolean;
  errorMessage: string | undefined;
};

export const previewState = $state<PreviewState>({
  entryId: undefined,
  preview: undefined,
  loading: false,
  errorMessage: undefined,
});

let previewInflight = 0;

export const hydratePreview = async (entryId: string | undefined): Promise<void> => {
  if (
    previewState.entryId === entryId &&
    (previewState.preview || previewState.loading || previewState.errorMessage)
  ) {
    return;
  }
  previewState.entryId = entryId;
  previewState.preview = undefined;
  previewState.errorMessage = undefined;
  if (!entryId || !isTauri()) {
    previewState.loading = false;
    return;
  }
  const ticket = ++previewInflight;
  previewState.loading = true;
  try {
    const preview = await getEntryPreview(entryId);
    if (ticket !== previewInflight || previewState.entryId !== entryId) return;
    previewState.preview = preview;
  } catch (err) {
    if (ticket !== previewInflight || previewState.entryId !== entryId) return;
    previewState.errorMessage = describeError(err);
  } finally {
    if (ticket === previewInflight && previewState.entryId === entryId) {
      previewState.loading = false;
    }
  }
};

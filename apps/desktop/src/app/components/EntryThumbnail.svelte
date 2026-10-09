<script lang="ts">
  import type { Snippet } from 'svelte';

  import { buildImageUrl } from '../lib/imageUrl';

  type Props = {
    entryId: string;
    // Accessible name. Empty for a decorative thumbnail whose row already
    // names the entry.
    alt: string;
    // `preview`: the supplementary image under a file list in the preview
    // pane. `row`: the small recall cue in a result row's leading column.
    variant: 'preview' | 'row';
    testId: string;
    // Rendered in place of a thumbnail that could not be loaded.
    fallback?: Snippet | undefined;
  };
  let { entryId, alt, variant, testId, fallback }: Props = $props();

  // The daemon serves `/thumb/<id>` for image-kind entries and for the image
  // render a file copy often carries (a presentation slide, a document page),
  // replying with 503 + Retry-After while it generates the cached copy on first
  // request. `<img onerror>` can't read the status, so we re-request a couple
  // of times on a fixed cadence before giving up. A thumbnail is purely
  // supplementary, so on persistent failure we drop it (or show `fallback`)
  // rather than a broken-image placeholder.
  const MAX_ATTEMPTS = 3;
  const RETRY_DELAY_MS = 1000;

  let attempt = $state(0);
  let failed = $state(false);
  let retryTimer: number | undefined = undefined;

  const src = $derived(failed ? undefined : buildImageUrl(entryId, true, attempt));

  // Reset the attempt ladder whenever a different entry is shown, and clear
  // any pending retry so a stale timer can't flip the new entry into a retry
  // it didn't ask for.
  $effect(() => {
    void entryId;
    attempt = 0;
    failed = false;
    return () => {
      if (retryTimer !== undefined) {
        window.clearTimeout(retryTimer);
        retryTimer = undefined;
      }
    };
  });

  function handleError(): void {
    if (attempt + 1 < MAX_ATTEMPTS) {
      // Pin the retry to the entry that errored. The reset effect above
      // already clears this timer when the entry changes, but capturing the
      // id makes the guarantee local: a retry fired for one entry can never
      // bump another entry's attempt ladder.
      const forEntry = entryId;
      retryTimer = window.setTimeout(() => {
        retryTimer = undefined;
        if (forEntry !== entryId) return;
        attempt += 1;
      }, RETRY_DELAY_MS);
      return;
    }
    failed = true;
  }
</script>

{#if src}
  <img
    class="thumb {variant}"
    data-testid={testId}
    {src}
    {alt}
    loading="lazy"
    decoding="async"
    onerror={handleError}
  />
{:else if fallback}
  {@render fallback()}
{/if}

<style>
  .thumb {
    display: block;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
  }
  /* Small, supplementary preview of a copied file. Capped tightly so it stays
     an affordance rather than dominating the pane — the file rows and the
     location below it must remain visible without scrolling. Aligned with the
     file rows' horizontal padding. */
  .preview {
    max-width: 100%;
    max-height: 120px;
    margin: 0.5rem 0.75rem 0;
    border-radius: 6px;
    object-fit: contain;
  }
  /* Fills the result row's fixed leading column without growing the row past
     its single text line, so image rows keep the list's row height. Cropped
     (`cover`) because at this size the cue is the colours and layout, which
     is what tells consecutive screenshots apart. */
  .row {
    box-sizing: border-box;
    width: 100%;
    height: 1.5rem;
    border-radius: 3px;
    object-fit: cover;
  }
</style>

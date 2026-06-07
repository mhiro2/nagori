<script lang="ts">
  import { buildImageUrl } from '../lib/imageUrl';

  type Props = {
    entryId: string;
    // Accessible name. The image is supplementary and its content is unknown
    // to us, so this is a generic localized label rather than a description.
    alt: string;
  };
  let { entryId, alt }: Props = $props();

  // A file copy often also places an image render of the copied object on the
  // clipboard (a presentation slide, a document page). The daemon serves it
  // from the same `/thumb/<id>` endpoint as image-kind entries, replying with
  // 503 + Retry-After while it generates the cached copy on first request.
  // `<img onerror>` can't read the status, so we re-request a couple of times
  // on a fixed cadence before giving up. The thumbnail is purely supplementary
  // — it is not guaranteed to faithfully represent the file — so on persistent
  // failure we drop it entirely rather than show a broken-image placeholder.
  const MAX_ATTEMPTS = 3;
  const RETRY_DELAY_MS = 1000;

  let attempt = $state(0);
  let failed = $state(false);
  let retryTimer: number | undefined = undefined;

  const src = $derived(failed ? undefined : buildImageUrl(entryId, true, attempt));

  // Reset the attempt ladder whenever a different entry is selected, and clear
  // any pending retry so a stale timer can't flip the newly-selected entry
  // into a retry it didn't ask for.
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
      // already clears this timer when the selection changes, but capturing
      // the id makes the guarantee local: a retry fired for one entry can
      // never bump another entry's attempt ladder.
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
    class="file-thumb"
    data-testid="preview-files-thumb"
    {src}
    {alt}
    loading="lazy"
    decoding="async"
    onerror={handleError}
  />
{/if}

<style>
  /* Small, supplementary preview of the copied file. Capped tightly so it stays
     an affordance rather than dominating the pane — the file rows and the
     location below it must remain visible without scrolling. Aligned with the
     file rows' horizontal padding. */
  .file-thumb {
    display: block;
    max-width: 100%;
    max-height: 120px;
    margin: 0.5rem 0.75rem 0;
    border-radius: 6px;
    border: 1px solid var(--border, rgba(128, 128, 128, 0.3));
    object-fit: contain;
  }
</style>

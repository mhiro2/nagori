<script lang="ts">
  type ImageBody = {
    type: 'image';
    mimeType?: string | null;
    byteCount: number;
    width?: number | null;
    height?: number | null;
  };

  type Props = {
    entryId: string;
    body: ImageBody;
    expanded: boolean;
    altText: string;
    unavailableText: string;
  };

  let { entryId, body, expanded, altText, unavailableText }: Props = $props();

  // Image bytes are streamed by the `nagori-image://` custom URI scheme
  // registered in src-tauri/src/lib.rs. In the inline preview we request
  // the daemon's cached 512px thumbnail (`/thumb/<id>`); the expanded
  // preview window switches to the original payload so a click-to-zoom
  // delivers full resolution. The Rust handler enforces sensitivity
  // gating on both paths.
  //
  // Attempt ladder for the non-expanded path:
  //   0 → first thumb fetch
  //   1 → thumb retry after a fixed 1s delay
  //   2 → original payload fallback (defends against an LRU-evicted or
  //       perma-skipped thumbnail row)
  //
  // The scheme handler returns 503 + Retry-After: 1 on a thumbnail miss
  // and kicks generation, but `<img onerror>` exposes neither the status
  // code nor headers — every error collapses to a single event. The
  // retry policy below is therefore a fixed frontend cadence, not an
  // observation of the spec'd header.
  let imageAttempt = $state(0);
  let retryTimer: number | undefined = undefined;
  const imageSrc = $derived.by((): string | undefined => {
    const useThumb = !expanded && imageAttempt < 2;
    return buildImageUrl(entryId, useThumb, imageAttempt);
  });
  const imageDimensions = $derived.by(() => {
    return body.width && body.height ? { width: body.width, height: body.height } : undefined;
  });
  let imageLoaded = $state(false);
  let imageFailed = $state(false);

  // Reset the skeleton + attempt ladder whenever a different entry is
  // selected or the user toggles into the expanded preview window. `void`
  // marks the dependency reads as intentional for the linter. The
  // cleanup clears any pending retry from the previous entry so a stale
  // timer can't flip the newly-selected row into retry state.
  $effect(() => {
    void entryId;
    void expanded;
    imageAttempt = 0;
    imageLoaded = false;
    imageFailed = false;
    return () => {
      if (retryTimer !== undefined) {
        window.clearTimeout(retryTimer);
        retryTimer = undefined;
      }
    };
  });

  function handleImageError(): void {
    if (expanded) {
      imageFailed = true;
      return;
    }
    if (imageAttempt === 0) {
      // First miss: the daemon almost certainly returned 503 and kicked
      // generation. Wait a fixed 1s, then re-request the same path with
      // a cache-busting query so the webview actually re-fetches.
      retryTimer = window.setTimeout(() => {
        retryTimer = undefined;
        imageAttempt = 1;
      }, 1000);
      return;
    }
    if (imageAttempt === 1) {
      // Retry also missed (slow decoder, oversized payload, LRU-evicted
      // mid-flight). Stream the original instead so the row still shows
      // something rather than the unavailable placeholder.
      imageAttempt = 2;
      return;
    }
    imageFailed = true;
  }

  function buildImageUrl(id: string, useThumb: boolean, attempt: number): string {
    // macOS / iOS / Linux origin: scheme://localhost/<path>
    // Windows / Android origin: http://<scheme>.localhost/<path>
    // We pick the platform-specific form so the webview's Origin matches the
    // fetched URL (otherwise SecurityError on Win/Android).
    const isWinAndroid =
      typeof navigator !== 'undefined' && /Windows|Android/i.test(navigator.userAgent);
    const origin = isWinAndroid ? 'http://nagori-image.localhost' : 'nagori-image://localhost';
    const segment = useThumb ? `thumb/${id}` : id;
    // The cache-buster only matters for the post-503 retry: without a
    // unique URL the webview may short-circuit the second fetch even
    // though the response was `Cache-Control: no-store`. The Rust
    // handler ignores the query string (`parse_image_entry_id` reads
    // only the path), so this is a free no-op for the first attempt.
    const suffix = attempt > 0 ? `?v=${attempt}` : '';
    return `${origin}/${segment}${suffix}`;
  }
</script>

{#if imageSrc && !imageFailed}
  <div class="image-frame" class:loaded={imageLoaded}>
    <img
      class="image"
      src={imageSrc}
      alt={altText}
      loading="lazy"
      decoding="async"
      width={imageDimensions?.width}
      height={imageDimensions?.height}
      onload={() => (imageLoaded = true)}
      onerror={handleImageError}
    />
  </div>
{:else}
  <p class="state" role="status">{unavailableText}</p>
{/if}

<style>
  .image-frame {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 80px;
    background: rgba(0, 0, 0, 0.4);
  }
  /* Checkerboard placeholder shown until the lazy <img> finishes decoding.
     Pure CSS so we never reference an external skeleton image (CSP-safe). */
  .image-frame:not(.loaded)::before {
    content: '';
    position: absolute;
    inset: 0;
    background-color: rgba(0, 0, 0, 0.2);
    background-image:
      linear-gradient(45deg, rgba(255, 255, 255, 0.06) 25%, transparent 25%),
      linear-gradient(-45deg, rgba(255, 255, 255, 0.06) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, rgba(255, 255, 255, 0.06) 75%),
      linear-gradient(-45deg, transparent 75%, rgba(255, 255, 255, 0.06) 75%);
    background-size: 16px 16px;
    background-position:
      0 0,
      0 8px,
      8px -8px,
      -8px 0;
    pointer-events: none;
  }
  .image {
    display: block;
    max-width: 100%;
    max-height: 100%;
    margin: 0 auto;
    object-fit: contain;
    /* Make the <img>'s intrinsic ratio drive layout when width/height attrs
       are present, so layout shift between skeleton and decoded image stays
       within the aspect ratio rather than collapsing to 0×0. */
    height: auto;
    width: auto;
  }
  .image-frame:not(.loaded) .image {
    opacity: 0;
  }
  .image-frame.loaded .image {
    opacity: 1;
    transition: opacity 120ms linear;
  }
  /* The image-unavailable fallback shares the `.state` visual primitive
     with the parent's loading/error rows. Duplicated here so the child
     keeps its own scoped style and the parent doesn't need to leak
     `.state` to globals. */
  .state {
    margin: 0;
    padding: 0.75rem;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-size: 0.8125rem;
  }
</style>

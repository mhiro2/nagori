<script lang="ts">
  import { tick } from 'svelte';

  import { buildImageUrl } from '../lib/imageUrl';
  import {
    isImeComposing,
    isPrimaryModifierHeld,
    paletteBindingsFor,
    resolveAction,
  } from '../lib/keybindings';
  import type { Binding } from '../lib/keybindings';
  import type { Platform } from '../lib/types';

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
    loadingText: string;
    // Host platform, so the keyboard zoom chord uses the primary modifier the
    // user actually presses (Cmd on macOS, Ctrl elsewhere). `undefined` until
    // the capability snapshot hydrates — `isPrimaryModifierHeld` treats that
    // as non-mac (Ctrl).
    platform?: Platform | undefined;
    // The palette's resolved key bindings. The zoom chord yields to any of them
    // so a user who remaps an action onto `Cmd+=`/`-`/`0` gets that action,
    // not a double-fire of zoom + the action (both listen on `window`).
    bindings?: readonly Binding[] | undefined;
  };

  let {
    entryId,
    body,
    expanded,
    altText,
    unavailableText,
    loadingText,
    platform,
    bindings,
  }: Props = $props();

  // Zoom for the expanded original payload, as a multiple of the fit-to-pane
  // size. Trackpad pinch / Ctrl-wheel drive it continuously; the keyboard
  // chords snap to discrete steps (so the readout lands on clean 150 % / 200 %
  // values); double-click toggles fit ↔ 2×. Zoom is applied by sizing the
  // scroll stage in CSS (not a `transform`), so the pane's `overflow: auto`
  // becomes real scroll-to-pan once the image is larger than the frame.
  // The named bounds also dodge a computed tuple index (which
  // `noUncheckedIndexedAccess` widens to `number | undefined`).
  const ZOOM_MIN = 1;
  const ZOOM_MAX = 8;
  const ZOOM_STEPS = [ZOOM_MIN, 1.5, 2, 3, 4, 6, ZOOM_MAX] as const;
  const DOUBLE_CLICK_ZOOM = 2;
  let zoom = $state(ZOOM_MIN);
  const zoomed = $derived(zoom > ZOOM_MIN);
  // The corner readout rounds to a whole percent for a clean label; the CSS
  // stage must NOT round — it scales by the continuous `zoom`, so the stage
  // size tracks `applyZoom`'s `clamped / prev` ratio exactly. Driving the stage
  // off the rounded value instead would let a sub-percent wheel step move the
  // scroll offset while the stage stayed put, drifting the anchored point.
  const zoomPercent = $derived(Math.round(zoom * 100));
  const zoomStagePercent = $derived(zoom * 100);

  const clampZoom = (value: number): number => Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, value));

  // The scroll-container element, used to attach the pointer zoom gestures and
  // to re-pin the scroll offset after a zoom (see `applyZoom`).
  let frameEl: HTMLDivElement | undefined = $state();

  // Set the zoom and keep the content point under (`clientX`, `clientY`) fixed
  // on screen, so the image grows toward the cursor rather than off the top-left
  // corner. The stage side is a flat `zoom%` of the frame, so content scales
  // linearly with zoom: a point at scroll offset `s + p` (p = pointer inside the
  // frame) moves to `(s + p) * next/prev`, and we subtract `p` again to leave it
  // under the same pixel. Pointer-less callers (the keyboard chord) pass no
  // coordinates and anchor on the frame centre. The stage resizes reactively, so
  // we wait a `tick` for that reflow before writing scroll — otherwise the target
  // exceeds the pre-resize scroll range and clamps to the wrong spot.
  async function applyZoom(next: number, clientX?: number, clientY?: number): Promise<void> {
    const prev = zoom;
    const clamped = clampZoom(next);
    if (clamped === prev) return;
    zoom = clamped;
    const el = frameEl;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const px = clientX === undefined ? rect.width / 2 : clientX - rect.left;
    const py = clientY === undefined ? rect.height / 2 : clientY - rect.top;
    const ratio = clamped / prev;
    const left = (el.scrollLeft + px) * ratio - px;
    const top = (el.scrollTop + py) * ratio - py;
    await tick();
    el.scrollLeft = left;
    el.scrollTop = top;
  }

  function zoomIn(): void {
    void applyZoom(ZOOM_STEPS.find((step) => step > zoom) ?? ZOOM_MAX);
  }
  function zoomOut(): void {
    let next: number = ZOOM_MIN;
    for (const step of ZOOM_STEPS) if (step < zoom) next = step;
    void applyZoom(next);
  }
  function resetZoom(): void {
    void applyZoom(ZOOM_MIN);
  }
  function toggleZoom(clientX?: number, clientY?: number): void {
    void applyZoom(zoomed ? ZOOM_MIN : DOUBLE_CLICK_ZOOM, clientX, clientY);
  }

  // Image bytes are streamed by the `nagori-image://` custom URI scheme
  // registered in src-tauri/src/lib.rs. In the inline preview we request
  // the daemon's cached 512px thumbnail (`/thumb/<id>`); the expanded
  // preview switches to the full-resolution original payload that the
  // keyboard zoom below scales. The Rust handler enforces sensitivity
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
    zoom = ZOOM_MIN;
    return () => {
      if (retryTimer !== undefined) {
        window.clearTimeout(retryTimer);
        retryTimer = undefined;
      }
    };
  });

  // While expanded, the primary-modifier zoom chords (⌘/Ctrl + `=`/`+` in,
  // `-` out, `0` fit) drive the zoom. The chord — rather than a bare key —
  // is deliberate: the search box keeps focus throughout the palette, and a
  // bare `0` / `-` there is an ordinary search character we must not swallow.
  // A modifier chord types nothing into the field, so the listener can live on
  // `window` and stay correct no matter where focus sits (or as the selection
  // remounts this component on entry navigation). Tauri ships with webview
  // zoom hotkeys disabled, so ⌘/Ctrl +/-/0 reach us instead of resizing the
  // whole UI; `preventDefault` guards the rest. The listener only exists while
  // an image is expanded (the component unmounts for every other kind).
  $effect(() => {
    if (!expanded) return;
    if (typeof window === 'undefined') return;
    const handler = (event: KeyboardEvent): void => {
      if (isImeComposing(event)) return;
      if (!isPrimaryModifierHeld(event, platform)) return;
      if (event.altKey) return;
      // Yield to the palette: if this chord is bound to a palette action (e.g.
      // a user remapped `delete` onto `Cmd+=`), let that action own it rather
      // than also zooming — both handlers sit on `window`, so `preventDefault`
      // can't stop the other. Fall back to the platform's default bindings so
      // the check stays correct (Ctrl-shaped on Windows/Linux) if a caller
      // omits `bindings`.
      if (resolveAction(event, bindings ?? paletteBindingsFor(platform))) return;
      switch (event.key) {
        case '+':
        case '=':
          zoomIn();
          break;
        case '-':
        case '_':
          zoomOut();
          break;
        case '0':
          resetZoom();
          break;
        default:
          return;
      }
      event.preventDefault();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  });

  // Pointer zoom on the expanded image. Three gestures, all scoped to the
  // frame so plain two-finger scrolling still pans the zoomed image. Each one
  // anchors the zoom on the gesture's own point (the wheel cursor, the pinch
  // centroid, the double-click) via `applyZoom`, so the image grows toward
  // where the user is acting rather than off the top-left corner:
  //   • trackpad pinch — WebKit (macOS/Linux WKWebView) delivers this as the
  //     non-standard `gesturechange` event with a cumulative `event.scale`,
  //     because wry leaves WKWebView's native magnification off. We multiply
  //     the zoom captured at `gesturestart` by that scale.
  //   • Ctrl/Cmd + wheel — the cross-platform fallback (this is exactly what a
  //     Chromium/WebView2 pinch emits, and what an explicit Ctrl-scroll does
  //     everywhere). A wheel without the modifier is left alone so it pans.
  //   • double-click — toggles fit ↔ 2×.
  // `passive: false` lets us `preventDefault` so the gesture zooms the image
  // instead of the page.
  $effect(() => {
    if (!expanded || !frameEl) return;
    const el = frameEl;
    let pinchBase = ZOOM_MIN;
    const onWheel = (event: WheelEvent): void => {
      if (!event.ctrlKey && !event.metaKey) return;
      event.preventDefault();
      void applyZoom(zoom * Math.pow(1.0015, -event.deltaY), event.clientX, event.clientY);
    };
    const onGestureStart = (event: Event): void => {
      event.preventDefault();
      pinchBase = zoom;
    };
    const onGestureChange = (event: Event): void => {
      event.preventDefault();
      const gesture = event as unknown as { scale?: number; clientX?: number; clientY?: number };
      void applyZoom(pinchBase * (gesture.scale ?? 1), gesture.clientX, gesture.clientY);
    };
    const onDoubleClick = (event: MouseEvent): void => {
      event.preventDefault();
      toggleZoom(event.clientX, event.clientY);
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    el.addEventListener('gesturestart', onGestureStart, { passive: false });
    el.addEventListener('gesturechange', onGestureChange, { passive: false });
    el.addEventListener('dblclick', onDoubleClick);
    return () => {
      el.removeEventListener('wheel', onWheel);
      el.removeEventListener('gesturestart', onGestureStart);
      el.removeEventListener('gesturechange', onGestureChange);
      el.removeEventListener('dblclick', onDoubleClick);
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
</script>

{#if imageSrc && !imageFailed}
  <div class="image-viewer" class:expanded>
    <div class="image-frame" class:expanded class:loaded={imageLoaded} bind:this={frameEl}>
      <div class="image-stage" style:--zoom-pct={zoomStagePercent}>
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
      {#if !imageLoaded}
        <!-- Explicit "loading" caption over the checkerboard skeleton. While a
             thumbnail miss is being (re)generated the <img> stays blank for up
             to ~1s per retry, so a worded status reads as "working on it"
             rather than a stuck/blank frame. -->
        <p class="overlay" role="status">{loadingText}</p>
      {/if}
    </div>
    {#if expanded}
      <!-- Current zoom level, always shown while expanded (including 100 %) so
           the readout doubles as a hint that the image is zoomable. A sibling of
           the (scrolling) frame, absolutely positioned on the non-scrolling
           viewer, so it stays pinned to the corner while a zoomed image pans.
           `role="status"` live region so a screen reader hears the level change
           as the user pinches / steps. -->
      <span class="zoom-level" role="status" aria-live="polite">{zoomPercent}%</span>
    {/if}
  </div>
{:else}
  <p class="state" role="status">{unavailableText}</p>
{/if}

<style>
  /* Inline preview: the viewer is transparent (`display: contents`) so the
     frame lays out exactly as before. Expanded: it becomes a flex column that
     fills the pane and parks the zoom bar below the scrollable frame. */
  .image-viewer {
    display: contents;
  }
  .image-viewer.expanded {
    position: relative;
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
  }
  .image-frame {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 80px;
    background: rgba(0, 0, 0, 0.4);
  }
  /* Expanded: the frame is the scroll container so a zoomed stage can be
     panned with the trackpad / scrollbar. A grid container plus the stage's
     `margin: auto` keeps a fit-sized image centred while staying scrollable
     from every edge once it overflows. */
  .image-viewer.expanded .image-frame {
    flex: 1;
    min-height: 0;
    display: grid;
    overflow: auto;
    /* The double-click zoom toggle would otherwise leave the OS text/range
       selection (the blue highlight) behind — `dblclick`'s preventDefault
       can't undo a selection the preceding mousedown sequence already made.
       Nothing in the frame is selectable text, so suppress selection here
       (WebKit/WKWebView needs the prefixed property). */
    user-select: none;
    -webkit-user-select: none;
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
  /* Inline: the stage is transparent so the <img> is a direct child of the
     frame (unchanged layout). Expanded: the stage is the zoom box — its side
     is `zoom%` of the frame (never below 100 %) so the frame overflows and
     scrolls once zoomed past fit. */
  .image-stage {
    display: contents;
  }
  .image-viewer.expanded .image-stage {
    display: grid;
    place-items: center;
    margin: auto;
    width: calc(var(--zoom-pct, 100) * 1%);
    height: calc(var(--zoom-pct, 100) * 1%);
    min-width: 100%;
    min-height: 100%;
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
  /* Expanded: fill the zoom stage (object-fit keeps the aspect) so the image
     scales uniformly with the stage — including upscaling small images past
     their natural size when zoomed in. */
  .image-viewer.expanded .image {
    width: 100%;
    height: 100%;
    max-width: none;
    max-height: none;
  }
  .image-frame:not(.loaded) .image {
    opacity: 0;
  }
  /* Centred "loading…" caption shown over the skeleton until the <img>
     decodes. Pointer-events off so it never intercepts clicks meant for the
     frame. */
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    margin: 0;
    padding: 0.5rem;
    color: var(--muted, rgba(255, 255, 255, 0.7));
    font-size: 0.8125rem;
    text-align: center;
    pointer-events: none;
  }
  .image-frame.loaded .image {
    opacity: 1;
    transition: opacity 120ms linear;
  }
  /* Current zoom level, pinned to the bottom-right corner. Absolute on the
     (position: relative) viewer — a sibling of the scrolling frame, not a child
     of it — so it stays in the corner while a zoomed image pans. Pointer-events
     off so it never eats a pinch / double-click meant for the image. */
  .zoom-level {
    position: absolute;
    right: 0.4rem;
    bottom: 0.4rem;
    padding: 0.05rem 0.4rem;
    border-radius: 999px;
    background: rgba(0, 0, 0, 0.55);
    color: var(--fg, #f5f5f5);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
    pointer-events: none;
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

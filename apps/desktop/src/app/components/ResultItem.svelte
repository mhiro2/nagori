<script module lang="ts">
  import { dedupedRepresentationLabels } from '../lib/representations';
  import type { RepresentationSummary } from '../lib/types';

  // Per-kind label for the leading badge. Keep these short so the column
  // stays a fixed width regardless of the active locale.
  const KIND_BADGE: Record<string, string> = {
    text: 'T',
    url: 'URL',
    code: '{ }',
    image: 'IMG',
    fileList: 'FILES',
    richText: 'RTF',
    unknown: '?',
  };
  const badge = (kind: string): string => KIND_BADGE[kind] ?? '?';

  const safeUrl = (raw: string): URL | undefined => {
    try {
      return new URL(raw.trim());
    } catch {
      return undefined;
    }
  };

  // Trailing "HTML + Plain" chip for rows that kept more than one format.
  // Single-format rows (the common case for plain text) get `undefined` so
  // the meta strip stays uncluttered.
  export const formatRepresentationBadge = (
    summary: readonly RepresentationSummary[] | undefined,
  ): string | undefined => {
    const labels = dedupedRepresentationLabels(summary);
    return labels.length > 1 ? labels.join(' + ') : undefined;
  };
</script>

<script lang="ts">
  import { codeLanguageLabel, detectCodeLanguage } from '../lib/codeLanguage';
  import {
    collapseWhitespace,
    formatByteCount,
    formatRelativeTime,
    truncatePreview,
  } from '../lib/formatting';
  import { messages } from '../lib/i18n/index.svelte';
  import { primaryRankReason, rankReasonLabel } from '../lib/rankReason';
  import { isScreenshotSource } from '../lib/screenshotSource';
  import type { SearchResultDto } from '../lib/types';
  import { domainCategory } from '../lib/urlCategory';
  import HighlightedText from './HighlightedText.svelte';

  type Props = {
    item: SearchResultDto;
    selected: boolean;
    marked?: boolean;
    index: number;
    // The query the current results were produced for (searchState.appliedQuery,
    // threaded through ResultList). Drives the in-row match highlight; empty /
    // undefined for the recent listing so those rows render plain.
    query?: string | undefined;
    onSelect: (index: number) => void;
    onConfirm: (index: number, event?: MouseEvent) => void;
    // `| undefined` is explicit so ResultList can forward its own optional
    // prop straight through under `exactOptionalPropertyTypes`.
    onTogglePin?: ((index: number) => void) | undefined;
    // Reference mode: the action inspector is open, so the selected row lifts
    // and the rest recede. Visual only — `onSelect` still fires on hover; the
    // palette decides whether to honour it.
    locked?: boolean | undefined;
  };

  const {
    item,
    selected,
    marked = false,
    index,
    query,
    onSelect,
    onConfirm,
    onTogglePin = () => {},
    locked = false,
  }: Props = $props();

  const t = $derived(messages());
  const previewText = $derived(truncatePreview(collapseWhitespace(item.preview)));
  const timeLabel = $derived(formatRelativeTime(item.createdAt));
  const url = $derived(item.kind === 'url' ? safeUrl(item.preview) : undefined);
  // Strong-brand badge for URL rows (GitHub / YouTube / …). Hostname only —
  // no network. `undefined` for unknown hosts so they just show the domain.
  const urlBrand = $derived(url ? domainCategory(url.hostname) : undefined);
  // Code language badge: prefer the daemon's canonical `language`; fall back
  // to a client-side sniff only for legacy rows that predate detection.
  const codeBadge = $derived(
    item.kind === 'code'
      ? (codeLanguageLabel(item.language) ?? codeLanguageLabel(detectCodeLanguage(item.preview)))
      : undefined,
  );
  // Image rows carry almost no preview text, so surface the pixel dimensions
  // (from a capture-time header probe) and the primary payload's byte size so
  // a screenshot is identifiable at a glance. Either may be absent on legacy
  // rows; the row degrades gracefully to whatever is known.
  const imageDims = $derived(
    item.kind === 'image' && item.imageWidth != null && item.imageHeight != null
      ? `${item.imageWidth}×${item.imageHeight}`
      : undefined,
  );
  const imageSize = $derived.by((): string | undefined => {
    if (item.kind !== 'image') return undefined;
    const primary = item.representationSummary.find((rep) => rep.role === 'primary');
    return primary ? formatByteCount(primary.byteCount) : undefined;
  });
  const isScreenshot = $derived(item.kind === 'image' && isScreenshotSource(item.sourceAppName));
  // File rows drop the representation chip: the row already carries the FILES
  // kind-badge, so a trailing "Files + PNG + Plain" rep-badge doubles "Files"
  // and steals horizontal room from the path. Other kinds keep the chip.
  const repBadge = $derived(
    item.kind === 'fileList' ? undefined : formatRepresentationBadge(item.representationSummary),
  );
  // Strongest *match* reason for this row. `undefined` for recent-listing rows
  // (empty query) so they stay chip-free; pinned state has its own 📌 column.
  const rankReason = $derived(primaryRankReason(item.rankReasons));
  const rankChip = $derived(
    rankReason !== undefined ? rankReasonLabel(rankReason, t.rankReason) : undefined,
  );
</script>

<div class="result-row" class:selected class:locked>
  <button
    type="button"
    class="result-item"
    class:selected
    class:marked
    role="option"
    aria-selected={selected}
    data-kind={item.kind}
    data-sensitivity={item.sensitivity}
    onmouseenter={() => onSelect(index)}
    onclick={(event) => onConfirm(index, event)}
  >
    <span class="multi-mark" aria-hidden="true">{marked ? '✓' : ''}</span>
    <span class="kind-badge" aria-hidden="true">{badge(item.kind)}</span>

    {#if url}
      <span class="preview url">
        {#if urlBrand}<span class="brand-badge" data-testid="url-brand">{urlBrand}</span>{/if}
        <span class="domain"><HighlightedText text={url.host} {query} /></span>
        <span class="path"><HighlightedText text={`${url.pathname}${url.search}`} {query} /></span>
      </span>
    {:else if item.kind === 'code'}
      <span class="preview code">
        {#if codeBadge}<span class="lang-badge">{codeBadge}</span>{/if}
        <code><HighlightedText text={previewText} {query} /></code>
      </span>
    {:else if item.kind === 'image'}
      <span class="preview image">
        {#if isScreenshot}<span class="shot-badge" title={t.palette.screenshotBadge}
            >{t.palette.screenshotBadge}</span
          >{/if}
        {#if imageDims}<span class="img-dims" data-testid="image-dims">{imageDims}</span>{/if}
        {#if imageSize}<span class="img-size">{imageSize}</span>{/if}
        {#if !imageDims && !imageSize}<span class="img-fallback"
            ><HighlightedText text={previewText} {query} /></span
          >{/if}
      </span>
    {:else}
      <span class="preview"><HighlightedText text={previewText} {query} /></span>
    {/if}

    <span class="meta">
      {#if rankChip}<span class="rank-chip" data-reason={rankReason} title={t.preview.fields.rank}
          >{rankChip}</span
        >{/if}
      {#if repBadge}<span class="rep-badge" title={t.preview.fields.formats}>{repBadge}</span>{/if}
      {#if item.sensitivity === 'Secret' || item.sensitivity === 'Blocked'}
        <span class="sens">{item.sensitivity}</span>
      {/if}
      {#if item.sourceAppName}<span class="source">{item.sourceAppName}</span>{/if}
      <span class="time">{timeLabel}</span>
    </span>
  </button>

  <!-- Dedicated trailing pin column. A sibling of the row button rather than a
       child so it stays a real <button> (no button-in-button), and clicking it
       toggles the pin without bubbling into the row's paste handler. Hidden
       until the row is hovered/selected (hover selects the row via the button's
       onmouseenter), shown solid once pinned — so the affordance is discoverable
       by mouse, not only via the ⌘P shortcut. -->
  <button
    type="button"
    class="pin-toggle"
    class:active={item.pinned}
    class:visible={selected && !item.pinned}
    tabindex="-1"
    aria-pressed={item.pinned}
    aria-label={t.keybindings.togglePin}
    title={t.keybindings.togglePin}
    onmouseenter={() => onSelect(index)}
    onclick={() => onTogglePin(index)}>📌</button
  >
</div>

<style>
  /* The row is a flex container so the pin toggle can sit in its own trailing
     column; the selection highlight lives here (not on the button) so it spans
     the pin column too and the row reads as one continuous selected band. */
  .result-row {
    display: flex;
    align-items: stretch;
    width: 100%;
    /* Smooth the recede when the action inspector opens/closes. */
    transition: opacity 0.12s ease;
    /* Skip layout/paint for rows scrolled out of view so a long result set
       (the palette caps the search limit at 50 today, but ResultList is
       reused for larger lists) stays smooth under arrow-key navigation.
       `contain-intrinsic-size` gives the browser a ~3rem placeholder height
       so the scrollbar and `scrollIntoView` math stay correct before an
       off-screen row is rendered. See ARCHITECTURE.md §12 for why we use this
       instead of windowing. */
    content-visibility: auto;
    contain-intrinsic-size: auto 3rem;
  }
  .result-row.selected {
    background: var(--bg-selected, rgba(120, 160, 255, 0.18));
  }
  /* Reference mode (action inspector open). The non-target rows recede so the
     list reads as "acting on this clip" rather than a live hover list, and the
     target row lifts above them with a soft shadow. Hover affordances stay
     suppressed here because the palette freezes hover selection while open, so
     no stale `:hover` can light a receded row. */
  .result-row.locked:not(.selected) {
    opacity: 0.4;
  }
  .result-row.locked.selected {
    position: relative;
    z-index: 1;
    box-shadow: 0 1px 8px rgba(0, 0, 0, 0.3);
  }
  /* Clicks are inert while open (the palette gates them in `handleConfirm`).
     Make the row itself non-interactive to the pointer so a click cannot even
     focus the row button: a focused list button would pull keyboard ownership
     away from the inspector, routing Enter/Escape to the palette's window
     handler (paste-and-close / plain-close) instead of the panel. The JS guard
     stays as defence in depth (and for environments that ignore this). */
  .result-row.locked .result-item {
    pointer-events: none;
  }
  /* In reference mode pinning is not part of the single-target action surface:
     the per-row pin is inert (`handleTogglePin` guards it too — a toggle would
     re-anchor the selection through `runQuery` and cancel the run). Drop
     pointer events on every locked row's pin and hide the hover-ghost so
     nothing reads as a live affordance; the solid 📌 on already-pinned rows
     stays as a state indicator. */
  .result-row.locked .pin-toggle {
    pointer-events: none;
  }
  .result-row.locked .pin-toggle:not(.active) {
    opacity: 0;
  }
  .result-item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex: 1 1 auto;
    min-width: 0;
    padding: 0.5rem 1rem;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .result-item.marked {
    box-shadow: inset 3px 0 0 var(--accent, #6c8dff);
  }
  /* Trailing pin affordance. Opacity is driven purely by reactive state, never
     by a bare `:hover`: only the selected row (a single row at any time, since
     hovering any part of a row — pin column included — selects it) shows its
     pin. A bare `.pin-toggle:hover` would reveal *hidden* rows' pins, and a
     stale `:hover` (WKWebView drops mouseleave on fast vertical moves) would
     leave several stuck solid at once. Scoping the hover bump to `.visible`
     means a hidden pin can never be lit by hover, so the reactive class is
     always the source of truth. Width is reserved unconditionally so revealing
     the ghost never shifts the row's layout. */
  .pin-toggle {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 2.5rem;
    padding: 0;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: 0.9rem;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.1s ease;
  }
  .pin-toggle.visible {
    opacity: 0.45;
  }
  .pin-toggle.active {
    opacity: 1;
  }
  /* Direct hover on the selected row's ghost pin commits it to full opacity as
     a "you're about to click this" cue. Gated on `.visible` so it can only
     ever brighten the one pin that is already showing. */
  .pin-toggle.visible:hover {
    opacity: 1;
  }
  .multi-mark {
    flex: none;
    width: 1.1rem;
    text-align: center;
    color: var(--accent, #6c8dff);
    font-size: 0.85rem;
    font-weight: 600;
  }
  .kind-badge {
    flex: none;
    width: 2.25rem;
    text-align: center;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.7rem;
    letter-spacing: 0.04em;
  }
  .preview {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg, #f5f5f5);
  }
  .preview.url {
    display: inline-flex;
    align-items: baseline;
    gap: 0.4rem;
  }
  .preview.url .domain {
    color: var(--accent, #6c8dff);
    font-weight: 600;
  }
  .preview.url .path {
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--muted, rgba(255, 255, 255, 0.6));
  }
  /* Strong-brand pill for known URL hosts (GitHub, YouTube, …). Filled accent
     so it reads as a recognised destination, distinct from the neutral domain
     text it sits beside. */
  .brand-badge {
    flex: none;
    align-self: center;
    padding: 0.05rem 0.4rem;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent, #6c8dff) 22%, transparent);
    color: var(--accent-soft, #b3c2ff);
    font-size: 0.65rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    white-space: nowrap;
  }
  .preview.code {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    overflow: hidden;
  }
  .preview.code code {
    overflow: hidden;
    text-overflow: ellipsis;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8125rem;
    color: var(--fg, #f5f5f5);
  }
  /* Code-language badge. Filled green pill (was a thin outline) so the
     language reads at a glance — Nagori leans developer-oriented and the
     language is a primary recall cue for code rows. */
  .lang-badge {
    flex: none;
    padding: 0.05rem 0.4rem;
    border-radius: 4px;
    background: color-mix(in srgb, var(--ok, #86d29a) 20%, transparent);
    color: var(--ok, #86d29a);
    font-size: 0.66rem;
    font-weight: 700;
    letter-spacing: 0.04em;
  }
  /* Image rows have no body text, so the dims/size/screenshot strip stands in
     for the preview. */
  .preview.image {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    overflow: hidden;
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-variant-numeric: tabular-nums;
  }
  .preview.image .img-dims {
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
    font-weight: 600;
  }
  .shot-badge {
    flex: none;
    padding: 0.05rem 0.4rem;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent, #6c8dff) 18%, transparent);
    color: var(--accent-soft, #b3c2ff);
    font-size: 0.65rem;
    font-weight: 600;
    white-space: nowrap;
  }
  .meta {
    display: flex;
    flex: none;
    align-items: center;
    gap: 0.5rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .sens {
    color: var(--warning, #f59e0b);
  }
  .rep-badge {
    padding: 0.05rem 0.35rem;
    border: 1px solid rgba(120, 160, 255, 0.35);
    border-radius: 4px;
    color: var(--accent, #6c8dff);
    font-size: 0.65rem;
    letter-spacing: 0.04em;
  }
  /* "Why did this match" chip. Neutral by default so it never reads as a
     warning; semantic / fuzzy hits get a distinct hue so they stand as a
     deliberate, equal-standing match type rather than a lesser one. */
  .rank-chip {
    flex: none;
    padding: 0.05rem 0.35rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.18));
    border-radius: 4px;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.7));
    font-size: 0.65rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .rank-chip[data-reason='SemanticMatch'],
  .rank-chip[data-reason='NgramMatch'] {
    border-color: rgba(150, 130, 230, 0.45);
    color: var(--accent-soft, #b3a4f0);
  }
</style>

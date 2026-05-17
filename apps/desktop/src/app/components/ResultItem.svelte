<script module lang="ts">
  import type { RepresentationSummary } from "../lib/types";

  // Per-kind label for the leading badge. Keep these short so the column
  // stays a fixed width regardless of the active locale.
  const KIND_BADGE: Record<string, string> = {
    text: "T",
    url: "URL",
    code: "{ }",
    image: "IMG",
    fileList: "FILES",
    richText: "RTF",
    unknown: "?",
  };
  const badge = (kind: string): string => KIND_BADGE[kind] ?? "?";

  // Cheap language sniff for the in-row badge. We deliberately keep this
  // shallower than the daemon's classifier — the goal is just to tell the
  // user "this looks like JSON / SQL / TS" at a glance.
  const CODE_HEURISTICS: ReadonlyArray<{ tag: string; pattern: RegExp }> = [
    { tag: "JSON", pattern: /^\s*[{[]/ },
    { tag: "TS", pattern: /\b(?:const|let|interface|type|import)\b/ },
    { tag: "RS", pattern: /\b(?:fn|impl|struct|enum|let mut)\b/ },
    { tag: "PY", pattern: /\b(?:def|import|class|self)\b/ },
    { tag: "SH", pattern: /^\s*(?:#!|\$ )/ },
    { tag: "SQL", pattern: /\b(?:select|insert|update|delete|create)\b/i },
    { tag: "HTML", pattern: /<\/?[a-z][^>]*>/i },
  ];

  const detectCodeLang = (preview: string): string | undefined => {
    for (const { tag, pattern } of CODE_HEURISTICS) {
      if (pattern.test(preview)) return tag;
    }
    return undefined;
  };

  const safeUrl = (raw: string): URL | undefined => {
    try {
      return new URL(raw.trim());
    } catch {
      return undefined;
    }
  };

  // Short label per MIME type so the row stays readable. We collapse
  // image alts to a single "IMG" tag and skip the primary representation
  // because the kind badge on the left already covers it — the goal is
  // just to flag "this clip also carried other formats".
  const REP_LABEL_BY_MIME: Record<string, string> = {
    "text/plain": "Plain",
    "text/html": "HTML",
    "application/rtf": "RTF",
    "text/uri-list": "Files",
    "image/png": "PNG",
    "image/jpeg": "JPEG",
    "image/gif": "GIF",
    "image/webp": "WebP",
    "image/tiff": "TIFF",
  };

  const representationLabel = (mime: string): string => {
    const known = REP_LABEL_BY_MIME[mime];
    if (known !== undefined) return known;
    if (mime.startsWith("image/")) return "IMG";
    return mime;
  };

  // Build the trailing "HTML + Plain" badge string from the stored
  // representation set. Returns `undefined` when an entry only kept its
  // primary representation, so the row stays uncluttered for the common
  // single-format case (most copied plain text never carries an HTML
  // alternative). Duplicates are deduped so a primary + plain_fallback
  // pair with the same MIME (which the publisher already collapses)
  // doesn't render twice.
  export const formatRepresentationBadge = (
    summary: readonly RepresentationSummary[] | undefined,
  ): string | undefined => {
    if (!summary || summary.length <= 1) return undefined;
    const seen = new Set<string>();
    const labels: string[] = [];
    for (const rep of summary) {
      const label = representationLabel(rep.mimeType);
      if (seen.has(label)) continue;
      seen.add(label);
      labels.push(label);
    }
    return labels.length > 1 ? labels.join(" + ") : undefined;
  };
</script>

<script lang="ts">
  import { collapseWhitespace, formatRelativeTime, truncatePreview } from "../lib/formatting";
  import type { SearchResultDto } from "../lib/types";

  type Props = {
    item: SearchResultDto;
    selected: boolean;
    marked?: boolean;
    index: number;
    onSelect: (index: number) => void;
    onConfirm: (index: number, event?: MouseEvent) => void;
  };

  const { item, selected, marked = false, index, onSelect, onConfirm }: Props = $props();

  const previewText = $derived(truncatePreview(collapseWhitespace(item.preview)));
  const timeLabel = $derived(formatRelativeTime(item.createdAt));
  const url = $derived(item.kind === "url" ? safeUrl(item.preview) : undefined);
  const codeLang = $derived(item.kind === "code" ? detectCodeLang(item.preview) : undefined);
  const repBadge = $derived(formatRepresentationBadge(item.representationSummary));
</script>

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
  <span class="multi-mark" aria-hidden="true">{marked ? "✓" : ""}</span>
  <span class="kind-badge" aria-hidden="true">{badge(item.kind)}</span>

  {#if url}
    <span class="preview url">
      <span class="domain">{url.host}</span>
      <span class="path">{url.pathname}{url.search}</span>
    </span>
  {:else if item.kind === "code"}
    <span class="preview code">
      {#if codeLang}<span class="lang-badge">{codeLang}</span>{/if}
      <code>{previewText}</code>
    </span>
  {:else}
    <span class="preview">{previewText}</span>
  {/if}

  <span class="meta">
    {#if repBadge}<span class="rep-badge" title="Preserved formats">{repBadge}</span>{/if}
    {#if item.pinned}<span class="pin" aria-label="pinned">📌</span>{/if}
    {#if item.sensitivity === "Secret" || item.sensitivity === "Blocked"}
      <span class="sens">{item.sensitivity}</span>
    {/if}
    {#if item.sourceAppName}<span class="source">{item.sourceAppName}</span>{/if}
    <span class="time">{timeLabel}</span>
  </span>
</button>

<style>
  .result-item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    width: 100%;
    padding: 0.5rem 1rem;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .result-item.selected {
    background: var(--bg-selected, rgba(120, 160, 255, 0.18));
  }
  .result-item.marked {
    box-shadow: inset 3px 0 0 var(--accent, #6c8dff);
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
  .preview.code {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    overflow: hidden;
  }
  .preview.code code {
    overflow: hidden;
    text-overflow: ellipsis;
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.8125rem;
    color: var(--fg, #f5f5f5);
  }
  .lang-badge {
    flex: none;
    padding: 0.05rem 0.35rem;
    border: 1px solid rgba(120, 200, 140, 0.4);
    border-radius: 4px;
    color: var(--ok, #86d29a);
    font-size: 0.65rem;
    letter-spacing: 0.04em;
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
</style>

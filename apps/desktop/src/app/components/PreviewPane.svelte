<script lang="ts">
  import { formatByteCount, formatRelativeTime } from "../lib/formatting";
  import { messages } from "../lib/i18n/index.svelte";
  import { dedupedRepresentationLabels } from "../lib/representations";
  import type { EntryPreviewDto, RepresentationSummary, SearchResultDto } from "../lib/types";
  import { tokenize, type Span } from "./tokenize";

  // Comma-joined "preserved formats" footer line. Only shown when an entry
  // kept more than its primary representation so single-format clips don't
  // clutter the row.
  const formatPreservedList = (
    summary: readonly RepresentationSummary[] | undefined,
  ): string | undefined => {
    const labels = dedupedRepresentationLabels(summary);
    return labels.length > 1 ? labels.join(", ") : undefined;
  };

  // `image/png` → `PNG`. Strip the `+xml` / `+json` structured-syntax suffix
  // so `image/svg+xml` renders as `SVG`. Used by the head summary chip.
  const formatImageMime = (mime: string | null | undefined): string | null => {
    if (!mime) return null;
    const slash = mime.indexOf("/");
    let subtype = slash < 0 ? mime : mime.slice(slash + 1);
    const plus = subtype.indexOf("+");
    if (plus > 0) subtype = subtype.slice(0, plus);
    if (!subtype) return null;
    return subtype.toUpperCase();
  };

  // Split on the last `/` or `\` so Windows-style file lists also light up
  // the basename emphasis. The dir portion keeps its trailing separator so
  // the visual order is "<dim>parent/</dim><strong>basename</strong>".
  // A path ending in a separator represents a directory; we strip the
  // trailing separator before splitting and return it in `trailing` so the
  // template can re-attach it to the basename (`foo/` rather than `foo`).
  const splitPath = (path: string): { dir: string; base: string; trailing: string } => {
    const lastChar = path.length > 0 ? path[path.length - 1] : "";
    const isDir = lastChar === "/" || lastChar === "\\";
    const body = isDir ? path.slice(0, -1) : path;
    const lastSlash = Math.max(body.lastIndexOf("/"), body.lastIndexOf("\\"));
    if (lastSlash < 0) return { dir: "", base: body, trailing: isDir ? lastChar : "" };
    return {
      dir: body.slice(0, lastSlash + 1),
      base: body.slice(lastSlash + 1),
      trailing: isDir ? lastChar : "",
    };
  };

  // Index just past the last separator that delimits parent-from-basename
  // in `s`, or 0 if none. A trailing separator (e.g. `/proj/build/`) is
  // treated as part of the directory's own name rather than as the
  // delimiter, so the parent extracted from `/proj/build/` is `/proj/` and
  // the entry can render under that header without becoming an empty row.
  const dirEndOf = (s: string): number => {
    const len = s.length;
    const limit =
      len > 0 && (s[len - 1] === "/" || s[len - 1] === "\\") ? len - 1 : len;
    const trunc = s.slice(0, limit);
    const last = Math.max(trunc.lastIndexOf("/"), trunc.lastIndexOf("\\"));
    return last < 0 ? 0 : last + 1;
  };

  // A lone filesystem root (`/`, `\`, or a Windows drive root like `C:\`)
  // is too noisy to surface as a common-parent header — every row would
  // still need its own absolute prefix to be readable. Collapse to `''`.
  const isRootOnlyPrefix = (s: string): boolean =>
    s === "/" || s === "\\" || /^[A-Za-z]:[\\/]$/.test(s);

  // Longest common directory prefix shared by every path in the list. We
  // compare each entry's *parent-directory candidate* (`dirEndOf`-trimmed
  // slice) rather than the raw path so the algorithm is order-independent —
  // otherwise a directory entry appearing later than its sibling file would
  // pin the prefix at the directory itself and collapse that row to empty.
  // Operates on character ranges between separators so we never split
  // inside a path segment.
  const findCommonParent = (paths: readonly string[]): string => {
    if (paths.length < 2) return "";
    const parents = paths.map((p) => p.slice(0, dirEndOf(p)));
    let prefix = parents[0]!;
    for (let i = 1; i < parents.length && prefix.length > 0; i += 1) {
      const parent = parents[i]!;
      while (prefix.length > 0 && !parent.startsWith(prefix)) {
        // Shrink to the next-shorter directory by dropping the trailing
        // separator and re-finding the previous one.
        const trimmed = prefix.slice(0, -1);
        prefix = trimmed.slice(0, dirEndOf(trimmed));
      }
    }
    if (isRootOnlyPrefix(prefix)) return "";
    return prefix;
  };

  // Map filename extensions to a small set of categories so the row can
  // sport a colour-coded dot without pulling in icon fonts. A path ending
  // in a separator is treated as a directory regardless of extension.
  const EXT_CATEGORY: Record<
    string,
    "image" | "code" | "archive" | "document"
  > = {
    png: "image", jpg: "image", jpeg: "image", gif: "image", webp: "image",
    svg: "image", bmp: "image", ico: "image", heic: "image", tiff: "image",
    tif: "image", avif: "image",
    ts: "code", tsx: "code", js: "code", jsx: "code", mjs: "code", cjs: "code",
    rs: "code", go: "code", py: "code", rb: "code", java: "code", kt: "code",
    swift: "code", c: "code", cpp: "code", cc: "code", h: "code", hpp: "code",
    cs: "code", php: "code", sh: "code", bash: "code", zsh: "code", sql: "code",
    json: "code", xml: "code", yaml: "code", yml: "code", toml: "code",
    html: "code", htm: "code", css: "code", scss: "code", sass: "code",
    less: "code", vue: "code", svelte: "code", md: "code", rst: "code",
    zip: "archive", tar: "archive", gz: "archive", tgz: "archive", bz2: "archive",
    xz: "archive", "7z": "archive", rar: "archive", dmg: "archive", iso: "archive",
    pdf: "document", doc: "document", docx: "document", xls: "document",
    xlsx: "document", ppt: "document", pptx: "document", txt: "document",
    rtf: "document", odt: "document", ods: "document", odp: "document",
    csv: "document", tsv: "document",
  };

  const classifyPath = (
    path: string,
  ): "image" | "code" | "archive" | "document" | "unknown" | "directory" => {
    const last = path.length > 0 ? path[path.length - 1] : "";
    if (last === "/" || last === "\\") return "directory";
    const lastSlash = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    const dot = path.lastIndexOf(".");
    // Leading-dot files (`.env`) and dots that live inside a parent dir
    // (`/some.dir/Makefile`) don't expose an extension worth colouring.
    if (dot <= lastSlash + 1) return "unknown";
    if (dot === path.length - 1) return "unknown";
    return EXT_CATEGORY[path.slice(dot + 1).toLowerCase()] ?? "unknown";
  };

  type Props = {
    item: SearchResultDto | undefined;
    preview: EntryPreviewDto | undefined;
    loading: boolean;
    errorMessage: string | undefined;
    expanded?: boolean;
    expandedLoading?: boolean;
    expandedErrorMessage?: string | undefined;
    onExpandBody?: (entryId: string) => void;
  };

  const {
    item,
    preview,
    loading,
    errorMessage,
    expanded = false,
    expandedLoading = false,
    expandedErrorMessage = undefined,
    onExpandBody,
  }: Props = $props();
  const t = $derived(messages());
  const bodyText = $derived(preview?.previewText ?? item?.preview ?? "");
  const preservedFormats = $derived(formatPreservedList(item?.representationSummary));
  const showHighlighting = $derived(
    preview !== undefined && (preview.body.type === "code" || preview.body.type === "url"),
  );
  const tokens = $derived(
    showHighlighting ? tokenize(bodyText, preview?.metadata.language ?? null) : [],
  );
  // Line numbers only make sense for the multi-line code body. The url body
  // shares the highlighter for inline URL colouring but stays single-line.
  const showLineNumbers = $derived(preview?.body.type === "code" && tokens.length > 0);
  const tokenLines = $derived<Span[][]>(showLineNumbers ? splitTokensByLine(tokens) : []);

  // Walk the token stream and break each token at every `\n`. Newlines become
  // line boundaries (dropped from the rendered span text — the `display:block`
  // on `.line` paints the break). Tokens that span multiple lines (block
  // comments, multi-line strings) emit one span per line with the same kind
  // so colouring is preserved across the gutter.
  function splitTokensByLine(allTokens: Span[]): Span[][] {
    const lines: Span[][] = [[]];
    for (const tok of allTokens) {
      const parts = tok.text.split("\n");
      for (let idx = 0; idx < parts.length; idx += 1) {
        if (idx > 0) lines.push([]);
        const part = parts[idx];
        if (part && part.length > 0) {
          lines[lines.length - 1]!.push({ kind: tok.kind, text: part });
        }
      }
    }
    return lines;
  }

  // Image bytes are streamed by the `nagori-image://` custom URI scheme
  // registered in src-tauri/src/lib.rs. The webview fetches the bytes lazily
  // as it would any other img src, so we don't pay the base64 + IPC tax for
  // every previewed row. The Rust handler enforces sensitivity gating.
  const imageSrc = $derived(
    preview?.body.type === "image" ? buildImageUrl(preview.id) : undefined,
  );
  const imageDimensions = $derived.by(() => {
    if (preview?.body.type !== "image") return undefined;
    const { width, height } = preview.body;
    return width && height ? { width, height } : undefined;
  });
  let imageLoaded = $state(false);
  let imageFailed = $state(false);
  // Reset the skeleton whenever a different image entry is selected so the
  // checkerboard reappears while the new bytes are streaming in. `void`
  // marks the dependency read as intentional for the linter.
  $effect(() => {
    void imageSrc;
    imageLoaded = false;
    imageFailed = false;
  });

  // Head summary chip: kind-specific one-liner that surfaces lineCount /
  // byteCount / dimensions / domain / file count without ever leaking
  // sensitive body bytes.
  const summaryChip = $derived.by((): string | undefined => {
    if (!preview) return undefined;
    const body = preview.body;
    switch (body.type) {
      case "text":
      case "code":
      case "richText":
      case "unknown": {
        const lines = t.preview.summary.lines(preview.metadata.lineCount);
        const bytes = formatByteCount(preview.metadata.byteCount);
        return `${lines} · ${bytes}`;
      }
      case "image": {
        return t.preview.summary.image({
          dimensions:
            body.width != null && body.height != null ? `${body.width}×${body.height}` : null,
          format: formatImageMime(body.mimeType ?? null),
          bytes: formatByteCount(body.byteCount),
        });
      }
      case "fileList": {
        return t.preview.fileList.summary(body.paths.length, body.total);
      }
      case "url": {
        return body.domain ?? preview.metadata.domain ?? undefined;
      }
    }
  });

  // Truncation note: branch on the DTO's structured `truncation` so the
  // head+tail variant can spell out the elided byte count. Falls back to
  // the legacy boolean for older payloads that lack `truncation` (e.g.
  // tests or non-bundled IPC consumers).
  const truncationNote = $derived.by((): string | undefined => {
    if (!preview) return undefined;
    const truncation = preview.metadata.truncation;
    if (truncation) {
      switch (truncation.kind) {
        case 'none':
          return undefined;
        case 'headOnly': {
          const shown = formatByteCount(Math.max(0, preview.metadata.byteCount - elidedBytes()));
          const total = formatByteCount(preview.metadata.byteCount);
          return t.preview.truncation.headOnly({ shown, total });
        }
        case 'headAndTail': {
          const elided = formatByteCount(truncation.elidedBytes);
          return t.preview.truncation.headAndTail({ elided });
        }
      }
    }
    return preview.metadata.truncated ? t.preview.truncated : undefined;
  });

  // For the headOnly fallback we don't get an explicit elided count, so we
  // infer it from `byteCount` vs. the standard 128 KiB cap (best-effort).
  function elidedBytes(): number {
    if (!preview) return 0;
    const trunc = preview.metadata.truncation;
    if (trunc?.kind === 'headAndTail') return trunc.elidedBytes;
    if (trunc?.kind === 'headOnly') return Math.max(0, preview.metadata.byteCount - 128 * 1024);
    return 0;
  }

  const showElidedMatchWarning = $derived(preview?.metadata.elidedContainsMatch === true);

  // Expand button only shows for Public, text-bearing bodies that were
  // actually trimmed. Image / fileList expansions are out of scope (the
  // bodies are not text and the IPC returns a richer DTO that we don't
  // re-render here).
  const canExpandBody = $derived.by((): boolean => {
    if (!preview) return false;
    if (!preview.metadata.fullContentAvailable) return false;
    if (!preview.metadata.truncated) return false;
    const kind = preview.body.type;
    return kind === 'text' || kind === 'code' || kind === 'richText' || kind === 'unknown';
  });

  // Number of paths hidden by the 50-row cap that the backend applies before
  // the DTO crosses the IPC boundary.
  const fileListOverflow = $derived.by((): number => {
    if (preview?.body.type !== "fileList") return 0;
    return Math.max(0, preview.body.total - preview.body.paths.length);
  });

  function buildImageUrl(entryId: string): string {
    // macOS / iOS / Linux origin: scheme://localhost/<path>
    // Windows / Android origin: http://<scheme>.localhost/<path>
    // We pick the platform-specific form so the webview's Origin matches the
    // fetched URL (otherwise SecurityError on Win/Android).
    const isWinAndroid =
      typeof navigator !== "undefined" && /Windows|Android/i.test(navigator.userAgent);
    return isWinAndroid
      ? `http://nagori-image.localhost/${entryId}`
      : `nagori-image://localhost/${entryId}`;
  }
</script>

<aside class="preview-pane" class:expanded>
  {#if item}
    <header class="head">
      <span class="kind">{preview?.title ?? item.kind}</span>
      <span class="time">{formatRelativeTime(item.createdAt)}</span>
    </header>
    {#if summaryChip}
      <p class="summary" data-testid="preview-summary">{summaryChip}</p>
    {/if}
    <div class="body-wrap">
      {#if loading}
        <p class="state">{t.preview.loading}</p>
      {:else if errorMessage}
        <p class="state error">{errorMessage}</p>
      {:else if preview?.body.type === "image"}
        {#if imageSrc && !imageFailed}
          <div class="image-frame" class:loaded={imageLoaded}>
            <img
              class="image"
              src={imageSrc}
              alt={t.preview.image.alt}
              loading="lazy"
              decoding="async"
              width={imageDimensions?.width}
              height={imageDimensions?.height}
              onload={() => (imageLoaded = true)}
              onerror={() => (imageFailed = true)}
            />
          </div>
        {:else}
          <p class="state" role="status">{t.preview.image.unavailable}</p>
        {/if}
      {:else if preview?.body.type === "fileList"}
        {@const commonParent = findCommonParent(preview.body.paths)}
        {#if commonParent}
          <p
            class="common-parent"
            data-testid="preview-files-common-parent"
            title={commonParent}
          >
            {t.preview.fileList.inFolder(commonParent)}
          </p>
        {/if}
        <ul class="files">
          {#each preview.body.paths as path (path)}
            {@const relative = commonParent ? path.slice(commonParent.length) : path}
            {@const parts = splitPath(relative)}
            {@const category = classifyPath(path)}
            <li title={path} class={`kind-${category}`}>
              <span class={`ext-dot ${category}`} aria-hidden="true"></span>
              {#if parts.dir}<span class="dim">{parts.dir}</span>{/if}<strong class="base"
                >{parts.base}{parts.trailing}</strong
              >
            </li>
          {/each}
          {#if fileListOverflow > 0}
            <li class="more" aria-live="polite">{t.preview.fileList.moreFiles(fileListOverflow)}</li>
          {/if}
        </ul>
      {:else if showLineNumbers}
        <pre class="body code with-lines"
          ><code>{#each tokenLines as line, lineIdx (lineIdx)}<span
              class="line"
              ><span class="lineno" aria-hidden="true"
              ></span>{#each line as tok, idx (idx)}<span class={tok.kind}
                >{tok.text}</span
              >{/each}</span
            >{/each}</code></pre>
      {:else if showHighlighting}
        <pre class="body code"><code>{#each tokens as tok, idx (idx)}<span class={tok.kind}>{tok.text}</span>{/each}</code></pre>
      {:else}
        <pre class="body">{bodyText}</pre>
      {/if}
    </div>
    {#if preview && truncationNote}
      <div class="truncation" data-testid="preview-truncation">
        <p class="note">{truncationNote}</p>
        {#if showElidedMatchWarning}
          <p class="note warn" role="status" data-testid="preview-elided-match">
            ⚠ {t.preview.truncation.elidedMatch}
          </p>
        {/if}
        {#if expanded && canExpandBody}
          <button
            type="button"
            class="expand"
            data-testid="preview-expand-button"
            disabled={expandedLoading}
            onclick={() => onExpandBody?.(preview.id)}
          >
            {expandedLoading ? t.preview.truncation.expanding : t.preview.truncation.expand}
          </button>
        {/if}
        {#if expandedErrorMessage}
          <p class="note error" role="alert">{expandedErrorMessage}</p>
        {/if}
      </div>
    {/if}
    <footer class="foot">
      <dl>
        <dt>{t.preview.fields.id}</dt>
        <dd>{item.id}</dd>
        <dt>{t.preview.fields.sensitivity}</dt>
        <dd>{item.sensitivity}</dd>
        {#if item.sourceAppName}
          <dt>{t.preview.fields.source}</dt>
          <dd>{item.sourceAppName}</dd>
        {/if}
        {#if preview}
          <dt>{t.preview.fields.size}</dt>
          <dd>{formatByteCount(preview.metadata.byteCount)}</dd>
        {/if}
        <dt>{t.preview.fields.rank}</dt>
        <dd>{item.rankReasons.join(", ") || t.preview.none}</dd>
        {#if preservedFormats}
          <dt>{t.preview.fields.formats}</dt>
          <dd>{preservedFormats}</dd>
        {/if}
      </dl>
    </footer>
  {:else}
    <p class="empty">{t.preview.empty}</p>
  {/if}
</aside>

<style>
  .preview-pane {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    width: 320px;
    padding: 1rem;
    border-left: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    background: var(--bg-elevated, rgba(255, 255, 255, 0.02));
    min-height: 0;
    overflow: hidden;
  }
  .preview-pane.expanded {
    border-left: none;
  }
  .head {
    display: flex;
    justify-content: space-between;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .summary {
    margin: 0;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
    overflow-wrap: anywhere;
  }
  .body-wrap {
    flex: 1;
    min-height: 0;
    overflow: auto;
    border-radius: 6px;
    background: var(--bg-code, rgba(0, 0, 0, 0.25));
  }
  .body {
    margin: 0;
    padding: 0.5rem;
    color: var(--fg, #f5f5f5);
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.8125rem;
    white-space: pre-wrap;
    word-break: break-word;
    /* Skip layout/paint for offscreen lines so very long previews don't
       block scroll. `contain-intrinsic-size` gives the browser a placeholder
       height before the offscreen subtree is rendered. */
    content-visibility: auto;
    contain-intrinsic-size: auto 1rem;
  }
  .state,
  .note {
    margin: 0;
    padding: 0.75rem;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-size: 0.8125rem;
  }
  .state.error,
  .note.error {
    color: var(--danger, #f87171);
  }
  .note {
    padding: 0;
  }
  .note.warn {
    color: var(--warn, #f5c97b);
  }
  .truncation {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .truncation .expand {
    align-self: flex-start;
    padding: 0.25rem 0.6rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 4px;
    background: transparent;
    color: var(--fg, #f5f5f5);
    font: inherit;
    font-size: 0.75rem;
    cursor: pointer;
  }
  .truncation .expand:disabled {
    opacity: 0.5;
    cursor: progress;
  }
  .truncation .expand:hover:not(:disabled) {
    background: var(--bg-elevated, rgba(255, 255, 255, 0.05));
  }
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
    content: "";
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
  /* Common-parent header sits above the list and shows the longest
     directory prefix shared by every row. Hover reveals the full prefix
     when middle-elided. */
  .common-parent {
    margin: 0;
    padding: 0.5rem 0.75rem 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.75rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .files {
    margin: 0;
    padding: 0.5rem 0.75rem;
    color: var(--fg, #f5f5f5);
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.8125rem;
    overflow-wrap: anywhere;
    list-style: none;
  }
  .files li {
    display: flex;
    align-items: baseline;
    gap: 0.45em;
  }
  .files .dim {
    color: var(--muted, rgba(255, 255, 255, 0.45));
  }
  .files .base {
    font-weight: 600;
    color: var(--fg, #f5f5f5);
  }
  /* Coloured dot communicating the extension category without pulling in
     icon fonts. aria-hidden on the span itself; the row's title attribute
     already carries the full path for screen readers. */
  .files .ext-dot {
    display: inline-block;
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    margin-top: 0.1em;
    border-radius: 50%;
    background-color: var(--muted, rgba(255, 255, 255, 0.4));
    align-self: center;
  }
  .files .ext-dot.image {
    background-color: var(--syntax-str, #f0a07b);
  }
  .files .ext-dot.code {
    background-color: var(--syntax-link, #7ec8ff);
  }
  .files .ext-dot.archive {
    background-color: var(--syntax-num, #f7c97a);
  }
  .files .ext-dot.document {
    background-color: var(--syntax-kw, #c08bff);
  }
  /* Directories drop the round shape so the badge reads as a folder edge
     rather than a file dot. */
  .files .ext-dot.directory {
    border-radius: 1px;
    background-color: var(--fg-secondary, rgba(255, 255, 255, 0.55));
  }
  .files .more {
    margin-top: 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-style: italic;
  }
  .body.code code {
    font: inherit;
  }
  .body :global(.kw) {
    color: var(--syntax-kw, #c08bff);
  }
  .body :global(.str) {
    color: var(--syntax-str, #f0a07b);
  }
  .body :global(.num) {
    color: var(--syntax-num, #f7c97a);
  }
  .body :global(.punct) {
    color: var(--syntax-punct, rgba(255, 255, 255, 0.55));
  }
  .body :global(.comment) {
    color: var(--syntax-comment, rgba(170, 170, 170, 0.7));
    font-style: italic;
  }
  .body :global(.link) {
    color: var(--syntax-link, #7ec8ff);
    text-decoration: underline;
    text-decoration-thickness: 1px;
    text-underline-offset: 2px;
  }
  /* Line-number gutter: CSS counter on each `.line` block; the `.lineno`
     element is aria-hidden so screen readers read the code only. */
  .body.code.with-lines code {
    counter-reset: line;
  }
  .body.code.with-lines :global(.line) {
    counter-increment: line;
    display: block;
  }
  .body.code.with-lines :global(.line .lineno)::before {
    content: counter(line);
    display: inline-block;
    width: 2.5em;
    margin-right: 0.75em;
    padding-right: 0.25em;
    color: var(--muted, rgba(255, 255, 255, 0.35));
    text-align: right;
    user-select: none;
    -webkit-user-select: none;
    border-right: 1px solid var(--border, rgba(255, 255, 255, 0.08));
  }
  .foot dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .foot dt {
    color: var(--muted, rgba(255, 255, 255, 0.4));
  }
  .foot dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .empty {
    color: var(--muted, rgba(255, 255, 255, 0.4));
    font-size: 0.875rem;
  }
</style>

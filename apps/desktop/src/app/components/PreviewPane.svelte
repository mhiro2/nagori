<script lang="ts">
  import { formatByteCount, formatRelativeTime } from "../lib/formatting";
  import { messages } from "../lib/i18n/index.svelte";
  import type { EntryPreviewDto, SearchResultDto } from "../lib/types";
  import { tokenize } from "./tokenize";

  type Props = {
    item: SearchResultDto | undefined;
    preview: EntryPreviewDto | undefined;
    loading: boolean;
    errorMessage: string | undefined;
    expanded?: boolean;
  };

  const { item, preview, loading, errorMessage, expanded = false }: Props = $props();
  const t = $derived(messages());
  const bodyText = $derived(preview?.previewText ?? item?.preview ?? "");
  const showHighlighting = $derived(
    preview !== undefined && (preview.body.type === "code" || preview.body.type === "url"),
  );
  const tokens = $derived(showHighlighting ? tokenize(bodyText) : []);

  // Image bytes are streamed by the `nagori-image://` custom URI scheme
  // registered in src-tauri/src/lib.rs. The webview fetches the bytes lazily
  // as it would any other img src, so we don't pay the base64 + IPC tax for
  // every previewed row. The Rust handler enforces sensitivity gating.
  const imageSrc = $derived(
    preview?.body.type === "image" ? buildImageUrl(preview.id) : undefined,
  );

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
    <div class="body-wrap">
      {#if loading}
        <p class="state">{t.preview.loading}</p>
      {:else if errorMessage}
        <p class="state error">{errorMessage}</p>
      {:else if preview?.body.type === "image"}
        {#if imageSrc}
          <img class="image" src={imageSrc} alt="" />
        {:else}
          <p class="state">{t.preview.image.unavailable}</p>
        {/if}
      {:else if preview?.body.type === "fileList"}
        <ul class="files">
          {#each preview.body.paths as path (path)}
            <li>{path}</li>
          {/each}
        </ul>
      {:else if showHighlighting}
        <pre class="body code"><code>{#each tokens as tok, idx (idx)}<span class={tok.kind}>{tok.text}</span>{/each}</code></pre>
      {:else}
        <pre class="body">{bodyText}</pre>
      {/if}
    </div>
    {#if preview?.metadata.truncated}
      <p class="note">{t.preview.truncated}</p>
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
  }
  .state,
  .note {
    margin: 0;
    padding: 0.75rem;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-size: 0.8125rem;
  }
  .state.error {
    color: var(--danger, #f87171);
  }
  .note {
    padding: 0;
  }
  .image {
    display: block;
    max-width: 100%;
    max-height: 100%;
    margin: 0 auto;
    object-fit: contain;
    background: rgba(0, 0, 0, 0.4);
  }
  .files {
    margin: 0;
    padding: 0.5rem 0.75rem 0.5rem 1.5rem;
    color: var(--fg, #f5f5f5);
    font-family:
      ui-monospace,
      SFMono-Regular,
      Menlo,
      monospace;
    font-size: 0.8125rem;
    overflow-wrap: anywhere;
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

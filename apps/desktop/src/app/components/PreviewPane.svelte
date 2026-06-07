<script lang="ts">
  import { formatByteCount, formatRelativeTime } from '../lib/formatting';
  import { messages } from '../lib/i18n/index.svelte';
  import { isImeComposing } from '../lib/keybindings';
  import type { Binding } from '../lib/keybindings';
  import { rankReasonLabels } from '../lib/rankReason';
  import { dedupedRepresentationLabels } from '../lib/representations';
  import type { EntryPreviewDto, RepresentationSummary, SearchResultDto } from '../lib/types';
  import { capabilitiesState } from '../stores/capabilities.svelte';
  import PreviewBodyFileList from './PreviewBodyFileList.svelte';
  import PreviewBodyImage from './PreviewBodyImage.svelte';
  import PreviewBodyText from './PreviewBodyText.svelte';
  import PreviewBodyUrl from './PreviewBodyUrl.svelte';
  import PreviewUrlConfirmDialog from './PreviewUrlConfirmDialog.svelte';

  // Renderer-side mirror of the backend `URL_SCHEME_ALLOWLIST`. The
  // `open_url_external` command re-validates this server-side so a
  // forged invoke can't escape — keeping the list here lets us hide
  // the trigger entirely when the user couldn't act on it anyway.
  const URL_OPEN_SCHEMES = new Set(['https', 'http']);

  // Comma-joined "preserved formats" footer line. Only shown when an entry
  // kept more than its primary representation so single-format clips don't
  // clutter the row.
  const formatPreservedList = (
    summary: readonly RepresentationSummary[] | undefined,
  ): string | undefined => {
    const labels = dedupedRepresentationLabels(summary);
    return labels.length > 1 ? labels.join(', ') : undefined;
  };

  // `image/png` → `PNG`. Strip the `+xml` / `+json` structured-syntax suffix
  // so `image/svg+xml` renders as `SVG`. Used by the head summary chip.
  const formatImageMime = (mime: string | null | undefined): string | null => {
    if (!mime) return null;
    const slash = mime.indexOf('/');
    let subtype = slash < 0 ? mime : mime.slice(slash + 1);
    const plus = subtype.indexOf('+');
    if (plus > 0) subtype = subtype.slice(0, plus);
    if (!subtype) return null;
    return subtype.toUpperCase();
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
    // Opens the action inspector for the previewed entry. When provided, the
    // header gains an "Actions" button so the quick actions are reachable by
    // mouse, not just the keyboard shortcut. Omitted (so the button is hidden)
    // in contexts that don't host the inspector.
    onOpenActions?: () => void;
    // Bindable: true while a plain Enter in the expanded preview will open
    // the URL, so the palette can stand down its Enter-to-paste binding and
    // the two handlers don't both fire on the same keystroke.
    enterOpensUrl?: boolean;
    // The query the visible results were produced for (searchState.appliedQuery),
    // forwarded to the text body so the preview marks the same hits as the row.
    query?: string | undefined;
    // Resolved palette key bindings, forwarded to the image body so its zoom
    // chord yields to any palette action mapped onto the same keys.
    bindings?: readonly Binding[] | undefined;
  };

  let {
    item,
    preview,
    loading,
    errorMessage,
    expanded = false,
    expandedLoading = false,
    expandedErrorMessage = undefined,
    onExpandBody,
    onOpenActions,
    enterOpensUrl = $bindable(false),
    query,
    bindings,
  }: Props = $props();
  const t = $derived(messages());
  const bodyText = $derived(preview?.previewText ?? item?.preview ?? '');
  const preservedFormats = $derived(formatPreservedList(item?.representationSummary));
  // Localised "why it matched" list, e.g. "Exact, Recent". Falls back to the
  // em-dash placeholder when a result somehow carries no reasons.
  const rankLabel = $derived(
    item ? rankReasonLabels(item.rankReasons, t.rankReason).join(', ') : '',
  );

  // Host platform for the expanded image's keyboard zoom chord (Cmd on macOS,
  // Ctrl elsewhere); pinch / Ctrl-wheel / double-click need no platform input.
  const imagePlatform = $derived(capabilitiesState.capabilities?.platform);

  // Head summary chip: kind-specific one-liner that surfaces lineCount /
  // byteCount / dimensions / domain / file count without ever leaking
  // sensitive body bytes.
  const summaryChip = $derived.by((): string | undefined => {
    if (!preview) return undefined;
    const body = preview.body;
    switch (body.type) {
      case 'text':
      case 'code':
      case 'richText':
      case 'unknown': {
        const lines = t.preview.summary.lines(preview.metadata.lineCount);
        const bytes = formatByteCount(preview.metadata.byteCount);
        return `${lines} · ${bytes}`;
      }
      case 'image': {
        return t.preview.summary.image({
          dimensions:
            body.width != null && body.height != null ? `${body.width}×${body.height}` : null,
          format: formatImageMime(body.mimeType ?? null),
          bytes: formatByteCount(body.byteCount),
        });
      }
      case 'fileList': {
        return t.preview.fileList.summary(body.entries.length, body.total);
      }
      case 'url': {
        // The dedicated URL layout already shows the host on its own row,
        // so the chip is redundant for URL kinds — leave it blank.
        return undefined;
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

  // URL-kind derived state. `urlBody` is `undefined` for non-URL bodies;
  // the template guards with `urlBody` so it never reaches the renderer
  // in that case.
  const urlBody = $derived(preview?.body.type === 'url' ? preview.body : undefined);
  // Gate the external-open trigger to Public + allowlisted-scheme URLs.
  // Backend re-checks this; the renderer keeps the hint and the button
  // hidden when the action would just bounce.
  const urlCanOpen = $derived.by((): boolean => {
    if (!urlBody) return false;
    if (item?.sensitivity !== 'Public') return false;
    const scheme = urlBody.scheme?.toLowerCase();
    return scheme !== undefined && URL_OPEN_SCHEMES.has(scheme);
  });
  // Body type used to decide which child component owns the body slot.
  // Computed once so the template stays readable.
  const bodyKind = $derived(preview?.body.type);
  const isCodeBody = $derived(bodyKind === 'code');
  // Fall back to `metadata.language` so older DTOs (or ones where the
  // body-level hint is missing) still drive syntax highlighting — matches
  // the pre-refactor source of truth.
  const codeLanguage = $derived(
    preview?.body.type === 'code'
      ? (preview.body.language ?? preview.metadata.language ?? null)
      : null,
  );

  // Confirm modal state. The renderer pops a curated dialog whose body
  // names the host so a renderer compromise can't silently re-direct
  // the user to an attacker URL while the dialog reads "example.com".
  let confirmOpenUrl = $state(false);

  // Enter-to-open owns the keystroke only inside the expanded preview, and
  // only for a plain Enter — modified Enter (paste-as-plain = ⌘⇧Enter,
  // copy = ⌘Enter) stays with the palette. Mirror that exact condition into
  // the bindable `enterOpensUrl` so the palette suppresses its own confirm
  // binding while we're the rightful Enter handler; otherwise both fire and
  // the URL opens *and* the entry pastes.
  $effect(() => {
    enterOpensUrl = expanded && urlCanOpen && !confirmOpenUrl;
  });
  // Attaches to `window` because the preview pane has no focused child by
  // default; the palette stands down its confirm binding (see
  // `enterOpensUrl`) so this window-scoped listener is the sole Enter
  // handler while expanded.
  $effect(() => {
    if (!expanded || !urlCanOpen) return;
    if (typeof window === 'undefined') return;
    const handler = (event: KeyboardEvent): void => {
      if (event.key !== 'Enter') return;
      // The Enter that commits an IME 変換 belongs to the focused search input,
      // not to opening the previewed URL.
      if (isImeComposing(event)) return;
      // Leave modified Enter to the palette's copy / paste-as-plain bindings.
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      if (confirmOpenUrl) return;
      event.preventDefault();
      confirmOpenUrl = true;
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  });
</script>

<aside class="preview-pane" class:expanded>
  {#if item}
    <header class="head">
      <span class="kind">{preview?.title ?? item.kind}</span>
      <span class="head-right">
        {#if onOpenActions}
          <button
            type="button"
            class="actions"
            data-testid="preview-open-actions"
            aria-label={t.actionMenu.title}
            title={t.actionMenu.title}
            onclick={onOpenActions}
          >
            {t.palette.hints.actions}
          </button>
        {/if}
        <span class="time">{formatRelativeTime(item.createdAt)}</span>
      </span>
    </header>
    {#if item.kind !== 'url'}
      <!-- Reserve the chip's line whenever the kind will carry one (every
           non-URL body), so the lines·bytes summary fades into pre-allocated
           space when the debounced preview fetch lands instead of shoving the
           body down a row. URL kinds intentionally have no chip. -->
      <p class="summary" class:pending={!summaryChip} data-testid="preview-summary">
        {summaryChip ?? ''}
      </p>
    {/if}
    <div class="body-wrap">
      {#if loading}
        <p class="state">{t.preview.loading}</p>
      {:else if errorMessage}
        <p class="state error">{errorMessage}</p>
      {:else if urlBody}
        <PreviewBodyUrl
          body={urlBody}
          canOpen={urlCanOpen}
          labels={{
            confirm: t.preview.url.confirm,
            punycodeBadge: t.preview.url.punycodeBadge,
            punycodeBadgeTitle: t.preview.url.punycodeBadgeTitle,
          }}
          onRequestOpen={() => {
            confirmOpenUrl = true;
          }}
        />
      {:else if preview && preview.body.type === 'image'}
        <PreviewBodyImage
          entryId={preview.id}
          body={preview.body}
          {expanded}
          altText={t.preview.image.alt}
          unavailableText={t.preview.image.unavailable}
          loadingText={t.preview.image.loading}
          platform={imagePlatform}
          {bindings}
        />
      {:else if preview && preview.body.type === 'fileList'}
        <PreviewBodyFileList
          entries={preview.body.entries}
          total={preview.body.total}
          commonParentDisplay={preview.body.commonParentDisplay}
          inFolderLabel={t.preview.fileList.inFolder}
          moreFilesLabel={t.preview.fileList.moreFiles}
          locationLabel={t.preview.fileList.location}
          fileRowAria={t.preview.fileList.fileRowAria}
        />
      {:else}
        <PreviewBodyText text={bodyText} language={codeLanguage} isCode={isCodeBody} {query} />
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
      {#if expanded && urlCanOpen}
        <p class="kbd-hint" data-testid="preview-url-open-hint">
          <kbd>Enter</kbd>
          {t.preview.url.openHint}
        </p>
      {/if}
      {#if item.sourceAppName}
        <!-- The one piece of provenance worth surfacing up front; the rest of
             the technical metadata folds into Details below. -->
        <dl class="primary">
          <dt>{t.preview.fields.source}</dt>
          <dd>{item.sourceAppName}</dd>
        </dl>
      {/if}
      <details class="details">
        <summary>{t.preview.details}</summary>
        <dl>
          <dt>{t.preview.fields.id}</dt>
          <dd>{item.id}</dd>
          <dt>{t.preview.fields.sensitivity}</dt>
          <dd>{item.sensitivity}</dd>
          <dt>{t.preview.fields.size}</dt>
          <dd>{preview ? formatByteCount(preview.metadata.byteCount) : ''}</dd>
          <dt>{t.preview.fields.rank}</dt>
          <dd>{rankLabel || t.preview.none}</dd>
          {#if preservedFormats}
            <dt>{t.preview.fields.formats}</dt>
            <dd>{preservedFormats}</dd>
          {/if}
        </dl>
      </details>
    </footer>
  {:else}
    <p class="empty">{t.preview.empty}</p>
  {/if}
  {#if confirmOpenUrl && urlBody && preview && urlCanOpen}
    <PreviewUrlConfirmDialog
      entryId={preview.id}
      body={urlBody}
      labels={{
        title: t.preview.url.confirmTitle,
        description: t.preview.url.confirmDescription,
        cancel: t.preview.url.cancel,
        confirm: t.preview.url.confirm,
        openFailed: t.preview.url.openFailed,
      }}
      onClose={() => {
        confirmOpenUrl = false;
      }}
    />
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
    align-items: center;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .head-right {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .actions {
    /* A mouse path to the inspector that mirrors the ⌘K shortcut. Sits in the
       uppercase head row but renders as a normal-case pill so it reads as an
       affordance, not a label. */
    padding: 0.1rem 0.5rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 999px;
    background: transparent;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
    font: inherit;
    font-size: 0.75rem;
    text-transform: none;
    letter-spacing: 0;
    cursor: pointer;
  }
  .actions:hover {
    background: color-mix(in srgb, var(--fg, #f5f5f5) 8%, transparent);
    color: var(--fg, #f5f5f5);
  }
  .actions:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  .summary {
    margin: 0;
    /* Reserve one line so the empty (pending) chip occupies the same height
       as the filled one — the lines·bytes value then appears without a shift. */
    min-height: 1.2em;
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
  .foot {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
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
  /* The technical fields (id / sensitivity / size / rank / formats) live in a
     collapsed disclosure so the resting pane leads with the body and the source
     line, not a wall of diagnostics. The grid above styles the inner <dl>. */
  .details summary {
    color: var(--muted, rgba(255, 255, 255, 0.45));
    font-size: 0.75rem;
    cursor: pointer;
    user-select: none;
  }
  .details summary:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  .details[open] summary {
    margin-bottom: 0.25rem;
  }
  .empty {
    color: var(--muted, rgba(255, 255, 255, 0.4));
    font-size: 0.875rem;
  }
  .kbd-hint {
    margin: 0 0 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .kbd-hint kbd {
    margin-right: 0.35em;
    padding: 0.05rem 0.3rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.16));
    border-radius: 3px;
    background: var(--bg-elevated, rgba(255, 255, 255, 0.06));
    font-family: inherit;
    font-size: 0.7rem;
  }
</style>

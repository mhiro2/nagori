<script lang="ts">
  import { categoryForExtension, type FileCategory } from '../lib/filePath';
  import type { FileEntry } from '../lib/types';
  import PreviewFileThumbnail from './PreviewFileThumbnail.svelte';

  type Props = {
    // Basename-first file rows, already split and home-folded by the backend.
    entries: readonly FileEntry[];
    total: number;
    // Home-folded directory prefix shared by every path; hoisted into a header.
    commonParentDisplay: string | null | undefined;
    inFolderLabel: (parent: string) => string;
    moreFilesLabel: (overflow: number) => string;
    // Visible label for the single-file "Location" row.
    locationLabel: string;
    // Accessible name for a file row (basename-first). `location` is the
    // parent directory, or null when the path has no parent segment.
    fileRowAria: (parts: { name: string; location: string | null }) => string;
    // Entry id, used to fetch the accompanying-image thumbnail.
    entryId: string;
    // True when the clip kept an image render alongside the file list; gates
    // the supplementary thumbnail at the top of the pane.
    hasImage: boolean;
    // Accessible name for that thumbnail.
    thumbnailAlt: string;
  };

  let {
    entries,
    total,
    commonParentDisplay,
    inFolderLabel,
    moreFilesLabel,
    locationLabel,
    fileRowAria,
    entryId,
    hasImage,
    thumbnailAlt,
  }: Props = $props();

  const startsWithSep = (s: string): boolean => s.startsWith('/') || s.startsWith('\\');
  const endsWithSep = (s: string): boolean => s.endsWith('/') || s.endsWith('\\');
  const sepOf = (s: string): string => (s.includes('\\') ? '\\' : '/');

  // Colour category for the row's dot. A trailing separator on the name is the
  // only directory hint a captured path carries and it is not reliable, so the
  // folder treatment stays a preview-local cosmetic (the dot squares off);
  // everything else maps the backend-supplied extension to a colour category.
  const dotCategory = (entry: FileEntry): FileCategory | 'directory' =>
    entry.kind === 'directory' || endsWithSep(entry.name)
      ? 'directory'
      : categoryForExtension(entry.extension);

  // The directory text to dim on a multi-file row: the entry's parent below the
  // hoisted header, with a trailing separator so it reads as "<sub>/<basename>".
  // Empty when the entry sits directly in the common parent (the row is then a
  // bare basename). A pure layout strip over the backend's home-folded display
  // strings — the parsing that produced them already happened server-side.
  const dimDir = (parentDisplay: string, common: string | null | undefined): string => {
    if (!parentDisplay) return '';
    if (common && parentDisplay === common) return '';
    let rel = parentDisplay;
    if (common && parentDisplay.startsWith(common)) {
      // Strip the shared header prefix when the entry sits below it. The
      // boundary character may be either separator (a path can mix `/` and
      // `\`), so test for a separator at the prefix edge rather than assuming
      // a single style for the whole string.
      const rest = parentDisplay.slice(common.length);
      if (startsWithSep(rest)) rel = rest.slice(1);
    }
    if (!rel) return '';
    return endsWithSep(rel) ? rel : rel + sepOf(rel);
  };

  // A single file gets a dedicated card: the basename is the heading and the
  // parent directory drops to its own "Location" row, so the filename is no
  // longer wedged onto the same line as a long absolute path. Multi-file
  // lists keep the common-parent header + per-row layout below.
  const single = $derived(total === 1 && entries.length === 1 ? entries[0]! : null);
  // Number of paths hidden by the per-row cap the backend applies before the
  // DTO crosses the IPC boundary.
  const overflow = $derived(Math.max(0, total - entries.length));
</script>

{#if hasImage}
  <PreviewFileThumbnail {entryId} alt={thumbnailAlt} />
{/if}
{#if single}
  <div class="single-file" data-testid="preview-files-single">
    <p class="single-head">
      <span class={`ext-dot ${dotCategory(single)}`} aria-hidden="true"></span>
      <strong class="base single-base">{single.name}</strong>
    </p>
    {#if single.parentDisplay}
      <dl class="single-meta">
        <dt class="loc-label">{locationLabel}</dt>
        <dd
          class="loc-value"
          data-testid="preview-files-location"
          title={single.parentRaw ?? single.parentDisplay}
        >
          {single.parentDisplay}
        </dd>
      </dl>
    {/if}
  </div>
{:else}
  {#if commonParentDisplay}
    <p class="common-parent" data-testid="preview-files-common-parent" title={commonParentDisplay}>
      {inFolderLabel(commonParentDisplay)}
    </p>
  {/if}
  <ul class="files">
    {#each entries as entry, i (i)}
      {@const dim = dimDir(entry.parentDisplay, commonParentDisplay)}
      {@const category = dotCategory(entry)}
      <!-- `title` is a mouse hover affordance only; screen readers do not
           announce it reliably, so the accessible name comes from the
           basename-first `aria-label`. -->
      <li
        title={entry.parentRaw ?? entry.parentDisplay}
        aria-label={fileRowAria({
          name: entry.name,
          location: entry.parentDisplay || null,
        })}
        class={`kind-${category}`}
      >
        <span class={`ext-dot ${category}`} aria-hidden="true"></span>
        {#if dim}<span class="dim">{dim}</span>{/if}<strong class="base">{entry.name}</strong>
      </li>
    {/each}
    {#if overflow > 0}
      <li class="more" aria-live="polite">
        {moreFilesLabel(overflow)}
      </li>
    {/if}
  </ul>
{/if}

<style>
  /* Common-parent header sits above the list and shows the longest
     directory prefix shared by every row. Hover reveals the full prefix
     when middle-elided. */
  .common-parent {
    margin: 0;
    padding: 0.5rem 0.75rem 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.75rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .files {
    margin: 0;
    padding: 0.5rem 0.75rem;
    color: var(--fg, #f5f5f5);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8125rem;
    list-style: none;
  }
  /* Each row stays on one line and truncates with an end ellipsis rather than
     wrapping the path character-by-character (the old `overflow-wrap: anywhere`,
     which scattered a long path across many hard-to-scan lines). The dimmed
     parent shrinks first so the emphasised basename survives. Segment-level
     middle elision (`~/Documents/…/Acme`) needs width measurement, so it is
     intentionally not attempted here. */
  .files li {
    display: flex;
    align-items: center;
    gap: 0.45em;
    min-width: 0;
    white-space: nowrap;
  }
  /* The dim parent carries a far larger flex-shrink than the basename, so
     negative free space is absorbed almost entirely by the parent first
     (shrink is distributed by shrink-factor × base-size). With `min-width: 0`
     the parent can ellipsize down to nothing before the emphasised basename
     starts to shrink; the basename keeps a shrink of 1 so a pathologically
     long name still ellipsizes rather than overflowing the row. */
  .files .dim {
    flex: 0 1000 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--muted, rgba(255, 255, 255, 0.45));
  }
  .files .base {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    font-weight: 600;
    color: var(--fg, #f5f5f5);
  }
  /* Single-file card: basename heading over a labelled Location row. */
  .single-file {
    padding: 0.5rem 0.75rem;
    color: var(--fg, #f5f5f5);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .single-head {
    display: flex;
    align-items: center;
    gap: 0.45em;
    margin: 0 0 0.5rem;
    min-width: 0;
  }
  .single-base {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.9375rem;
    font-weight: 600;
    color: var(--fg, #f5f5f5);
  }
  .single-meta {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0;
    font-size: 0.75rem;
  }
  .single-meta .loc-label {
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.55));
  }
  .single-meta .loc-value {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
  }
  /* Coloured dot communicating the extension category without pulling in
     icon fonts. The dot is `aria-hidden`; the row's accessible name comes
     from its basename-first `aria-label` (the `title` is hover-only and not
     reliably announced by screen readers). */
  .files .ext-dot,
  .single-head .ext-dot {
    display: inline-block;
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    margin-top: 0.1em;
    border-radius: 50%;
    background-color: var(--muted, rgba(255, 255, 255, 0.4));
    align-self: center;
  }
  .files .ext-dot.image,
  .single-head .ext-dot.image {
    background-color: var(--syntax-str, #f0a07b);
  }
  .files .ext-dot.code,
  .single-head .ext-dot.code {
    background-color: var(--syntax-link, #7ec8ff);
  }
  .files .ext-dot.archive,
  .single-head .ext-dot.archive {
    background-color: var(--syntax-num, #f7c97a);
  }
  .files .ext-dot.document,
  .single-head .ext-dot.document {
    background-color: var(--syntax-kw, #c08bff);
  }
  /* Directories drop the round shape so the badge reads as a folder edge
     rather than a file dot. */
  .files .ext-dot.directory,
  .single-head .ext-dot.directory {
    border-radius: 1px;
    background-color: var(--fg-secondary, rgba(255, 255, 255, 0.55));
  }
  .files .more {
    margin-top: 0.25rem;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-style: italic;
  }
</style>

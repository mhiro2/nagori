<script lang="ts">
  import {
    classifyExtension,
    findCommonParent,
    parentForDisplay,
    splitPath,
    type FileCategory,
  } from '../lib/filePath';

  type Props = {
    paths: readonly string[];
    total: number;
    inFolderLabel: (parent: string) => string;
    moreFilesLabel: (overflow: number) => string;
    // Visible label for the single-file "Location" row.
    locationLabel: string;
    // Accessible name for a file row (basename-first). `location` is the
    // parent directory, or null when the path has no parent segment.
    fileRowAria: (parts: { name: string; location: string | null }) => string;
  };

  let { paths, total, inFolderLabel, moreFilesLabel, locationLabel, fileRowAria }: Props = $props();

  // Colour category for the row's dot. A trailing separator is the only
  // directory hint a captured path carries and it is not reliable, so the
  // folder treatment stays a preview-local cosmetic (the dot squares off);
  // everything else defers to the shared extension classifier.
  const classifyRow = (path: string): FileCategory | 'directory' => {
    const last = path.length > 0 ? path[path.length - 1] : '';
    if (last === '/' || last === '\\') return 'directory';
    return classifyExtension(path);
  };

  const commonParent = $derived(findCommonParent(paths));
  // Number of paths hidden by the 50-row cap that the backend applies before
  // the DTO crosses the IPC boundary.
  const overflow = $derived(Math.max(0, total - paths.length));

  // A single file gets a dedicated card: the basename is the heading and the
  // parent directory drops to its own "Location" row, so the filename is no
  // longer wedged onto the same line as a long absolute path. Multi-file
  // lists keep the common-parent header + per-row layout below.
  const single = $derived(total === 1 && paths.length === 1 ? splitPath(paths[0]!) : null);
  const singleCategory = $derived(paths.length === 1 ? classifyRow(paths[0]!) : 'unknown');
  const singleLocation = $derived(single && single.dir ? parentForDisplay(single.dir) : '');
</script>

{#if single}
  <div class="single-file" data-testid="preview-files-single">
    <p class="single-head">
      <span class={`ext-dot ${singleCategory}`} aria-hidden="true"></span>
      <strong class="base single-base">{single.base}{single.trailing}</strong>
    </p>
    {#if singleLocation}
      <dl class="single-meta">
        <dt class="loc-label">{locationLabel}</dt>
        <dd class="loc-value" data-testid="preview-files-location" title={single.dir}>
          {singleLocation}
        </dd>
      </dl>
    {/if}
  </div>
{:else}
  {#if commonParent}
    <p class="common-parent" data-testid="preview-files-common-parent" title={commonParent}>
      {inFolderLabel(commonParent)}
    </p>
  {/if}
  <ul class="files">
    {#each paths as path (path)}
      {@const relative = commonParent ? path.slice(commonParent.length) : path}
      {@const parts = splitPath(relative)}
      {@const full = splitPath(path)}
      {@const category = classifyRow(path)}
      <!-- `title` is a mouse hover affordance only; screen readers do not
           announce it reliably, so the accessible name comes from the
           basename-first `aria-label`. -->
      <li
        title={path}
        aria-label={fileRowAria({
          name: `${full.base}${full.trailing}`,
          location: full.dir ? parentForDisplay(full.dir) : null,
        })}
        class={`kind-${category}`}
      >
        <span class={`ext-dot ${category}`} aria-hidden="true"></span>
        {#if parts.dir}<span class="dim">{parts.dir}</span>{/if}<strong class="base"
          >{parts.base}{parts.trailing}</strong
        >
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

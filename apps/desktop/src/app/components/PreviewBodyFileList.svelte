<script lang="ts">
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

  // Split on the last `/` or `\` so Windows-style file lists also light up
  // the basename emphasis. The dir portion keeps its trailing separator so
  // the visual order is "<dim>parent/</dim><strong>basename</strong>".
  // A path ending in one or more separators represents a directory; we strip
  // the whole trailing run before splitting and return a single representative
  // separator in `trailing` so the template can re-attach it to the basename
  // (`foo/` rather than `foo`). Collapsing the run keeps a non-normalised
  // `…/dir//` from yielding an empty basename.
  const splitPath = (path: string): { dir: string; base: string; trailing: string } => {
    const body = path.replace(/[/\\]+$/, '');
    const isDir = body.length < path.length;
    // The first stripped char stands in for the (possibly repeated) trailing run.
    const trailing = isDir ? path.charAt(body.length) : '';
    const lastSlash = Math.max(body.lastIndexOf('/'), body.lastIndexOf('\\'));
    if (lastSlash < 0) return { dir: '', base: body, trailing };
    return {
      dir: body.slice(0, lastSlash + 1),
      base: body.slice(lastSlash + 1),
      trailing,
    };
  };

  // Index just past the last separator that delimits parent-from-basename
  // in `s`, or 0 if none. A trailing separator (e.g. `/proj/build/`) is
  // treated as part of the directory's own name rather than as the
  // delimiter, so the parent extracted from `/proj/build/` is `/proj/` and
  // the entry can render under that header without becoming an empty row.
  const dirEndOf = (s: string): number => {
    const len = s.length;
    const limit = len > 0 && (s[len - 1] === '/' || s[len - 1] === '\\') ? len - 1 : len;
    const trunc = s.slice(0, limit);
    const last = Math.max(trunc.lastIndexOf('/'), trunc.lastIndexOf('\\'));
    return last < 0 ? 0 : last + 1;
  };

  // A lone filesystem root (`/`, `\`, or a Windows drive root like `C:\`)
  // is too noisy to surface as a common-parent header — every row would
  // still need its own absolute prefix to be readable. Collapse to `''`.
  const isRootOnlyPrefix = (s: string): boolean =>
    s === '/' || s === '\\' || /^[A-Za-z]:[\\/]$/.test(s);

  // Longest common directory prefix shared by every path in the list. We
  // compare each entry's *parent-directory candidate* (`dirEndOf`-trimmed
  // slice) rather than the raw path so the algorithm is order-independent —
  // otherwise a directory entry appearing later than its sibling file would
  // pin the prefix at the directory itself and collapse that row to empty.
  // Operates on character ranges between separators so we never split
  // inside a path segment.
  const findCommonParent = (input: readonly string[]): string => {
    if (input.length < 2) return '';
    const parents = input.map((p) => p.slice(0, dirEndOf(p)));
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
    if (isRootOnlyPrefix(prefix)) return '';
    return prefix;
  };

  // Map filename extensions to a small set of categories so the row can
  // sport a colour-coded dot without pulling in icon fonts. A path ending
  // in a separator is treated as a directory regardless of extension.
  const EXT_CATEGORY: Record<string, 'image' | 'code' | 'archive' | 'document'> = {
    png: 'image',
    jpg: 'image',
    jpeg: 'image',
    gif: 'image',
    webp: 'image',
    svg: 'image',
    bmp: 'image',
    ico: 'image',
    heic: 'image',
    tiff: 'image',
    tif: 'image',
    avif: 'image',
    ts: 'code',
    tsx: 'code',
    js: 'code',
    jsx: 'code',
    mjs: 'code',
    cjs: 'code',
    rs: 'code',
    go: 'code',
    py: 'code',
    rb: 'code',
    java: 'code',
    kt: 'code',
    swift: 'code',
    c: 'code',
    cpp: 'code',
    cc: 'code',
    h: 'code',
    hpp: 'code',
    cs: 'code',
    php: 'code',
    sh: 'code',
    bash: 'code',
    zsh: 'code',
    sql: 'code',
    json: 'code',
    xml: 'code',
    yaml: 'code',
    yml: 'code',
    toml: 'code',
    html: 'code',
    htm: 'code',
    css: 'code',
    scss: 'code',
    sass: 'code',
    less: 'code',
    vue: 'code',
    svelte: 'code',
    md: 'code',
    rst: 'code',
    zip: 'archive',
    tar: 'archive',
    gz: 'archive',
    tgz: 'archive',
    bz2: 'archive',
    xz: 'archive',
    '7z': 'archive',
    rar: 'archive',
    dmg: 'archive',
    iso: 'archive',
    pdf: 'document',
    doc: 'document',
    docx: 'document',
    xls: 'document',
    xlsx: 'document',
    ppt: 'document',
    pptx: 'document',
    txt: 'document',
    rtf: 'document',
    odt: 'document',
    ods: 'document',
    odp: 'document',
    csv: 'document',
    tsv: 'document',
  };

  const classifyPath = (
    path: string,
  ): 'image' | 'code' | 'archive' | 'document' | 'unknown' | 'directory' => {
    const last = path.length > 0 ? path[path.length - 1] : '';
    if (last === '/' || last === '\\') return 'directory';
    const lastSlash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
    const dot = path.lastIndexOf('.');
    // Leading-dot files (`.env`) and dots that live inside a parent dir
    // (`/some.dir/Makefile`) don't expose an extension worth colouring.
    if (dot <= lastSlash + 1) return 'unknown';
    if (dot === path.length - 1) return 'unknown';
    return EXT_CATEGORY[path.slice(dot + 1).toLowerCase()] ?? 'unknown';
  };

  const commonParent = $derived(findCommonParent(paths));
  // Number of paths hidden by the 50-row cap that the backend applies before
  // the DTO crosses the IPC boundary.
  const overflow = $derived(Math.max(0, total - paths.length));

  // Parent directory formatted as a location: drop the trailing separator
  // (`/tmp/` → `/tmp`) so it reads as a place, but keep a filesystem root
  // intact — `/`, `\`, and `C:\` are meaningful as-is and `C:\` must not
  // collapse to the drive-relative `C:`.
  const parentForDisplay = (dir: string): string =>
    isRootOnlyPrefix(dir) ? dir : dir.replace(/[/\\]+$/, '');

  // A single file gets a dedicated card: the basename is the heading and the
  // parent directory drops to its own "Location" row, so the filename is no
  // longer wedged onto the same line as a long absolute path. Multi-file
  // lists keep the common-parent header + per-row layout below.
  const single = $derived(total === 1 && paths.length === 1 ? splitPath(paths[0]!) : null);
  const singleCategory = $derived(paths.length === 1 ? classifyPath(paths[0]!) : 'unknown');
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
      {@const category = classifyPath(path)}
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

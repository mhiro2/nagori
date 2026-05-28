<script lang="ts">
  type Props = {
    paths: readonly string[];
    total: number;
    inFolderLabel: (parent: string) => string;
    moreFilesLabel: (overflow: number) => string;
  };

  let { paths, total, inFolderLabel, moreFilesLabel }: Props = $props();

  // Split on the last `/` or `\` so Windows-style file lists also light up
  // the basename emphasis. The dir portion keeps its trailing separator so
  // the visual order is "<dim>parent/</dim><strong>basename</strong>".
  // A path ending in a separator represents a directory; we strip the
  // trailing separator before splitting and return it in `trailing` so the
  // template can re-attach it to the basename (`foo/` rather than `foo`).
  const splitPath = (path: string): { dir: string; base: string; trailing: string } => {
    const lastChar = path.length > 0 ? path[path.length - 1] : '';
    const isDir = lastChar === '/' || lastChar === '\\';
    const body = isDir ? path.slice(0, -1) : path;
    const lastSlash = Math.max(body.lastIndexOf('/'), body.lastIndexOf('\\'));
    if (lastSlash < 0) return { dir: '', base: body, trailing: isDir ? lastChar : '' };
    return {
      dir: body.slice(0, lastSlash + 1),
      base: body.slice(lastSlash + 1),
      trailing: isDir ? lastChar : '',
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
</script>

{#if commonParent}
  <p class="common-parent" data-testid="preview-files-common-parent" title={commonParent}>
    {inFolderLabel(commonParent)}
  </p>
{/if}
<ul class="files">
  {#each paths as path (path)}
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
  {#if overflow > 0}
    <li class="more" aria-live="polite">
      {moreFilesLabel(overflow)}
    </li>
  {/if}
</ul>

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
</style>

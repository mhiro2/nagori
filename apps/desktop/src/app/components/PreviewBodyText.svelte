<script lang="ts">
  import HighlightedText from './HighlightedText.svelte';
  import { tokenize, type Span } from './tokenize';

  type Props = {
    text: string;
    language: string | null;
    // Drives the syntax highlight + line-number gutter rendering. Non-code
    // bodies (text / richText / unknown) collapse to a `<pre>` carrying only
    // the query-match highlight.
    isCode: boolean;
    // The query the preview belongs to (searchState.appliedQuery), so the
    // body marks the same hits the result row does. Applied to non-code bodies
    // only; code bodies keep their grammar colouring instead. `highlightQuery`
    // caps its own scan, so a large body stays bounded.
    query?: string | undefined;
  };

  let { text, language, isCode, query }: Props = $props();

  const tokens = $derived(isCode ? tokenize(text, language) : []);
  // Line numbers only make sense for the multi-line code body. The url body
  // shares the highlighter for inline URL colouring but stays single-line.
  const showLineNumbers = $derived(isCode && tokens.length > 0);
  const tokenLines = $derived<Span[][]>(showLineNumbers ? splitTokensByLine(tokens) : []);

  // Walk the token stream and break each token at every `\n`. Newlines become
  // line boundaries (dropped from the rendered span text — the `display:block`
  // on `.line` paints the break). Tokens that span multiple lines (block
  // comments, multi-line strings) emit one span per line with the same kind
  // so colouring is preserved across the gutter.
  function splitTokensByLine(allTokens: Span[]): Span[][] {
    const lines: Span[][] = [[]];
    for (const tok of allTokens) {
      const parts = tok.text.split('\n');
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
</script>

{#if showLineNumbers}
  <pre class="body code with-lines"><code
      >{#each tokenLines as line, lineIdx (lineIdx)}<span class="line"
          ><span class="lineno" aria-hidden="true"></span>{#each line as tok, idx (idx)}<span
              class={tok.kind}>{tok.text}</span
            >{/each}</span
        >{/each}</code
    ></pre>
{:else if isCode}
  <pre class="body code"><code
      >{#each tokens as tok, idx (idx)}<span class={tok.kind}>{tok.text}</span>{/each}</code
    ></pre>
{:else}
  <pre class="body"><HighlightedText {text} {query} /></pre>
{/if}

<style>
  .body {
    margin: 0;
    padding: 0.5rem;
    color: var(--fg, #f5f5f5);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8125rem;
    white-space: pre-wrap;
    word-break: break-word;
    /* Skip layout/paint for offscreen lines so very long previews don't
       block scroll. `contain-intrinsic-size` gives the browser a placeholder
       height before the offscreen subtree is rendered. */
    content-visibility: auto;
    contain-intrinsic-size: auto 1rem;
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
</style>

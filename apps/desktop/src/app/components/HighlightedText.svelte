<script lang="ts">
  import { highlightQuery } from '../lib/highlightQuery';

  type Props = {
    text: string;
    // The query the visible results were produced for (searchState.appliedQuery),
    // not the live keystroke query — so a row's highlight matches the list it
    // belongs to. `undefined` / empty renders the text with no marks.
    query: string | undefined;
  };

  let { text, query }: Props = $props();

  const segments = $derived(highlightQuery(text, query));
</script>

<!-- Each segment is rendered with plain text interpolation (never `@html`), so
     untrusted clipboard bodies can never inject markup. -->
{#each segments as segment, index (index)}{#if segment.match}<mark class="match"
      >{segment.text}</mark
    >{:else}{segment.text}{/if}{/each}

<style>
  /* Query-match highlight. Soft accent wash + inherited colour so it reads as
     "this is where your search landed" without fighting the surrounding text
     (URL accent, code monospace, muted preview, …). */
  mark.match {
    padding: 0 0.05em;
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent, #6c8dff) 32%, transparent);
    color: inherit;
  }
</style>

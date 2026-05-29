<script lang="ts">
  import { formatRelativeTime } from '../lib/formatting';
  import { messages } from '../lib/i18n/index.svelte';
  import type { SearchResultDto } from '../lib/types';

  // A light-weight summary of the entry an action runs against: kind, source
  // app, and relative time on one line, with a few lines of the captured
  // snippet below. Unlike `PreviewPane` this never fetches the full body — it
  // reads only the `SearchResultDto` the palette already has, so the target
  // stays visible the instant the menu opens.
  type Props = {
    item: SearchResultDto | undefined;
  };

  const { item }: Props = $props();
  const t = $derived(messages());

  // Drop the parts the entry doesn't carry (e.g. a clip with no source app)
  // so the separators never frame an empty slot.
  const metaParts = $derived(
    item
      ? [item.kind, item.sourceAppName, formatRelativeTime(item.createdAt)].filter(
          (part): part is string => Boolean(part),
        )
      : [],
  );
</script>

<section class="target" data-testid="action-target">
  {#if item}
    <p class="meta">
      {#each metaParts as part, index (index)}
        {#if index > 0}<span class="sep" aria-hidden="true">·</span>{/if}<span>{part}</span>
      {/each}
    </p>
    <p class="snippet">{item.preview}</p>
  {:else}
    <p class="empty">{t.preview.empty}</p>
  {/if}
</section>

<style>
  .target {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.4rem;
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .sep {
    color: var(--muted, rgba(255, 255, 255, 0.3));
  }
  .snippet {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    overflow: hidden;
    margin: 0;
    color: var(--fg, #f5f5f5);
    font-size: 0.8125rem;
    line-height: 1.45;
    overflow-wrap: anywhere;
  }
  .empty {
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.4));
    font-size: 0.8125rem;
  }
</style>

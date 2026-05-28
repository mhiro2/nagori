<script lang="ts">
  type UrlBody = {
    type: 'url';
    url: string;
    domain?: string | null;
    scheme?: string | null;
    hostDisplay?: string | null;
    hostPunycode?: string | null;
    pathAndQuery?: string | null;
  };

  type Labels = {
    confirm: string;
    punycodeBadge: string;
    punycodeBadgeTitle: (args: { ascii: string }) => string;
  };

  type Props = {
    body: UrlBody;
    canOpen: boolean;
    labels: Labels;
    onRequestOpen: () => void;
  };

  let { body, canOpen, labels, onRequestOpen }: Props = $props();

  // Show the punycode badge when the backend signals an IDN mismatch
  // (display Unicode host differs from the ASCII xn-- form). The hover
  // title carries the raw ASCII so the user can verify against an
  // external source.
  const punycode = $derived(body.hostPunycode ?? null);
</script>

<div class="url-body" data-testid="preview-url-body">
  <p class="url-host" data-testid="preview-url-host">
    {body.hostDisplay ?? body.domain ?? body.url}
    {#if punycode}
      <span
        class="punycode-badge"
        role="status"
        data-testid="preview-url-punycode-badge"
        title={labels.punycodeBadgeTitle({ ascii: punycode })}
      >
        ⚠ {labels.punycodeBadge}
      </span>
    {/if}
  </p>
  {#if body.scheme || body.pathAndQuery}
    <p class="url-path" data-testid="preview-url-path">
      {#if body.scheme}<span class="dim">{body.scheme}://</span>{/if}
      <span title={body.pathAndQuery ?? body.url}>
        {body.pathAndQuery ?? ''}
      </span>
    </p>
  {/if}
  {#if canOpen}
    <button
      type="button"
      class="url-open"
      data-testid="preview-url-open-button"
      onclick={onRequestOpen}
    >
      {labels.confirm}
    </button>
  {/if}
</div>

<style>
  .url-body {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.75rem;
  }
  .url-host {
    margin: 0;
    color: var(--fg, #f5f5f5);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 1rem;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .url-path {
    margin: 0;
    color: var(--fg-secondary, rgba(255, 255, 255, 0.72));
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.8125rem;
    overflow-wrap: anywhere;
  }
  .url-path .dim {
    color: var(--muted, rgba(255, 255, 255, 0.5));
  }
  .punycode-badge {
    display: inline-block;
    margin-left: 0.5em;
    padding: 0.05rem 0.4rem;
    border: 1px solid var(--warn, #f5c97b);
    border-radius: 999px;
    color: var(--warn, #f5c97b);
    font-size: 0.7rem;
    font-weight: 500;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    vertical-align: middle;
  }
  .url-open {
    align-self: flex-start;
    margin-top: 0.25rem;
    padding: 0.3rem 0.75rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 4px;
    background: transparent;
    color: var(--fg, #f5f5f5);
    font: inherit;
    font-size: 0.8125rem;
    cursor: pointer;
  }
  .url-open:hover {
    background: var(--bg-elevated, rgba(255, 255, 255, 0.06));
  }
</style>

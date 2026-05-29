<script lang="ts">
  // A single flat list of actions — deterministic and AI alike. AI entries
  // carry a small badge rather than a separate section or an `AI:` label
  // prefix, so the user scans by intent ("what do I want done?") instead of
  // by implementation. The parent owns all state; this component only paints
  // the buttons and forwards clicks.
  type PickerItem = {
    // Stable key, also used as the button's `data-testid`.
    key: string;
    label: string;
    isAi: boolean;
    disabled: boolean;
    // Hover hint shown when the entry is disabled (e.g. AI unavailable).
    reason?: string | undefined;
    // True while this action is the one in flight, for the inline spinner.
    pending: boolean;
    run: () => void;
  };

  type Props = {
    items: PickerItem[];
    aiBadge: string;
    // Tightened spacing once a run/result occupies the work area below.
    compact?: boolean;
  };

  const { items, aiBadge, compact = false }: Props = $props();
</script>

<div class="picker" class:compact data-testid="action-picker">
  {#each items as item (item.key)}
    <button
      type="button"
      class="action"
      class:ai={item.isAi}
      data-testid={item.key}
      disabled={item.disabled}
      title={item.reason}
      aria-label={item.isAi ? `${item.label}, ${aiBadge}` : item.label}
      onclick={item.run}
    >
      <span class="label">{item.label}</span>
      {#if item.isAi}<span class="badge" aria-hidden="true">{aiBadge}</span>{/if}
      {#if item.pending}<span class="spinner" aria-hidden="true"></span>{/if}
    </button>
  {/each}
</div>

<style>
  .picker {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 0.5rem;
  }
  .picker.compact {
    gap: 0.375rem;
  }
  .action {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    padding: 0.5rem 0.75rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 6px;
    background: var(--bg-elevated, rgba(255, 255, 255, 0.03));
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition:
      background 0.12s ease,
      border-color 0.12s ease;
  }
  .picker.compact .action {
    padding: 0.375rem 0.625rem;
    font-size: 0.8125rem;
  }
  .action:hover:not(:disabled) {
    background: color-mix(in srgb, var(--fg, #f5f5f5) 8%, transparent);
    border-color: color-mix(in srgb, var(--fg, #f5f5f5) 16%, transparent);
  }
  .action:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  .action:active:not(:disabled) {
    transform: translateY(1px);
  }
  .action:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .label {
    flex: 1;
    min-width: 0;
  }
  .badge {
    flex: none;
    padding: 0.05rem 0.4rem;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent, #6c8dff) 22%, transparent);
    color: var(--accent, #6c8dff);
    font-size: 0.625rem;
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .spinner {
    flex: none;
    width: 0.7rem;
    height: 0.7rem;
    border: 2px solid color-mix(in srgb, var(--fg, #f5f5f5) 25%, transparent);
    border-top-color: var(--accent, #6c8dff);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
    }
    .action:active:not(:disabled) {
      transform: none;
    }
  }
</style>

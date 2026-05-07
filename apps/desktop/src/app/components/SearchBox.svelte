<script lang="ts">
  import { messages } from "../lib/i18n/index.svelte";

  type Props = {
    value: string;
    placeholder?: string;
    onInput: (next: string) => void;
    onKeydown?: (event: KeyboardEvent) => void;
  };

  const { value, placeholder, onInput, onKeydown }: Props = $props();

  const effectivePlaceholder = $derived(placeholder ?? messages().palette.placeholder);

  let inputEl: HTMLInputElement | undefined = $state();

  $effect(() => {
    inputEl?.focus();
  });

  const handleInput = (event: Event): void => {
    const next = (event.currentTarget as HTMLInputElement).value;
    onInput(next);
  };
</script>

<div class="search-box">
  <span class="prompt" aria-hidden="true">›</span>
  <input
    bind:this={inputEl}
    class="search-input"
    type="text"
    spellcheck="false"
    autocomplete="off"
    autocapitalize="off"
    autocorrect="off"
    placeholder={effectivePlaceholder}
    {value}
    oninput={handleInput}
    onkeydown={onKeydown}
  />
</div>

<style>
  .search-box {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    background: var(--bg-elevated, rgba(255, 255, 255, 0.03));
  }
  .prompt {
    color: var(--muted, rgba(255, 255, 255, 0.4));
    font-size: 1rem;
  }
  .search-input {
    flex: 1;
    background: transparent;
    border: none;
    outline: none;
    color: var(--fg, #f5f5f5);
    font-size: 1rem;
    font-family: inherit;
  }
  .search-input::placeholder {
    color: var(--muted, rgba(255, 255, 255, 0.4));
  }
</style>

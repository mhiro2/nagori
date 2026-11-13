<script lang="ts">
  // Names an entry from the action inspector. A name is the user's own word for
  // the entry ("PR review request", "shipping address"); aliases are extra
  // words that should find it too. The parent owns the IPC; this form only
  // edits the two fields and reports what was submitted.
  import type { EntryLabel } from '../lib/types';

  type Labels = {
    title: string;
    name: string;
    aliases: string;
    aliasesHint: string;
    save: string;
    remove: string;
    cancel: string;
    pinNote: string;
  };

  type Props = {
    // The current name, or `null` when the entry has none.
    initial: EntryLabel | null;
    labels: Labels;
    busy: boolean;
    errorMessage?: string | undefined;
    onSave: (name: string, aliases: string[]) => void;
    onRemove: () => void;
    onCancel: () => void;
  };

  const { initial, labels, busy, errorMessage, onSave, onRemove, onCancel }: Props = $props();

  // Seeded once from the label loaded when the form opened; the parent
  // remounts the form for another entry.
  // svelte-ignore state_referenced_locally
  let name = $state(initial?.name ?? '');
  // svelte-ignore state_referenced_locally
  let aliasesText = $state(initial?.aliases.join(', ') ?? '');

  // Commas (ASCII or ideographic) and line breaks separate aliases.
  const parseAliases = (value: string): string[] =>
    value
      .split(/[,、，\n]/)
      .map((alias) => alias.trim())
      .filter((alias) => alias !== '');

  const submit = (event: SubmitEvent): void => {
    event.preventDefault();
    if (busy || name.trim() === '') return;
    onSave(name, parseAliases(aliasesText));
  };

  let nameInput: HTMLInputElement | undefined = $state();
  $effect(() => {
    nameInput?.focus();
  });
</script>

<form class="label-form" data-testid="entry-label-form" onsubmit={submit}>
  <span class="title">{labels.title}</span>
  <label class="field">
    <span>{labels.name}</span>
    <input
      bind:this={nameInput}
      bind:value={name}
      data-testid="entry-label-name"
      maxlength="80"
      autocomplete="off"
      spellcheck="false"
    />
  </label>
  <label class="field">
    <span>{labels.aliases}</span>
    <input
      bind:value={aliasesText}
      data-testid="entry-label-aliases"
      autocomplete="off"
      spellcheck="false"
      aria-describedby="entry-label-aliases-hint"
    />
    <small id="entry-label-aliases-hint">{labels.aliasesHint}</small>
  </label>
  {#if !initial}
    <p class="note">{labels.pinNote}</p>
  {/if}
  {#if errorMessage}
    <p class="error" role="alert">{errorMessage}</p>
  {/if}
  <div class="buttons">
    {#if initial}
      <button type="button" class="ghost" disabled={busy} onclick={onRemove}>{labels.remove}</button
      >
    {/if}
    <button type="button" class="ghost" onclick={onCancel}>{labels.cancel}</button>
    <button type="submit" class="primary" disabled={busy || name.trim() === ''}
      >{labels.save}</button
    >
  </div>
</form>

<style>
  .label-form {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .title {
    color: var(--muted, rgba(255, 255, 255, 0.6));
    font-size: 0.8125rem;
    font-weight: 600;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.8125rem;
  }
  .field input {
    padding: 0.4rem 0.55rem;
    border: 1px solid var(--border-strong, rgba(255, 255, 255, 0.18));
    border-radius: 6px;
    background: var(--bg-input, var(--bg, transparent));
    color: var(--fg, #f5f5f5);
    font: inherit;
  }
  .field input:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  small,
  .note {
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.5));
    font-size: 0.75rem;
  }
  .error {
    margin: 0;
    color: var(--danger, #f87171);
    font-size: 0.8125rem;
    overflow-wrap: anywhere;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 0.4rem;
  }
  .ghost,
  .primary {
    padding: 0.25rem 0.7rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.12));
    border-radius: 6px;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: 0.8125rem;
    cursor: pointer;
  }
  .primary {
    border-color: var(--accent, #6c8dff);
    background: color-mix(in srgb, var(--accent-wash, #6c8dff) 22%, transparent);
  }
  .ghost:focus-visible,
  .primary:focus-visible {
    outline: 2px solid var(--accent, #6c8dff);
    outline-offset: 1px;
  }
  .ghost:disabled,
  .primary:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>

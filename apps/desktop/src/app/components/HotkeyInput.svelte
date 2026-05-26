<script lang="ts">
  import {
    captureFromKeyboardEvent,
    formatAccelerator,
    type CaptureTarget,
  } from '../lib/keybindings';
  import type { Platform } from '../lib/types';

  type Props = {
    value: string;
    platform: Platform | undefined;
    target: CaptureTarget;
    placeholder?: string;
    recordingLabel: string;
    clearLabel: string;
    onChange: (next: string) => void;
    // Optional id for label association — the surrounding `<label>` element
    // in SettingsView wires the visible text up to the input via implicit
    // labelling, but screen readers also benefit from explicit `id`s on
    // composite controls.
    id?: string;
  };

  let { value, platform, target, placeholder, recordingLabel, clearLabel, onChange, id }: Props =
    $props();

  let recording = $state(false);
  let buttonEl: HTMLButtonElement | undefined = $state();

  const formatted = $derived(value ? formatAccelerator(value, platform) : '');

  const startRecording = (): void => {
    recording = true;
    buttonEl?.focus();
  };

  const stopRecording = (): void => {
    recording = false;
  };

  const handleKeyDown = (event: KeyboardEvent): void => {
    if (!recording) {
      // While idle, Space / Enter on the focused button arms recording —
      // matches the standard button activation semantics so keyboard-only
      // users don't have to mouse-click the field.
      if (event.key === ' ' || event.key === 'Enter') {
        event.preventDefault();
        startRecording();
      }
      return;
    }
    // While recording every keystroke is consumed so the OS / palette can't
    // see partial combos. The native shortcut registration only commits
    // when a non-modifier key arrives; until then the surrounding form
    // stays inert.
    event.preventDefault();
    event.stopPropagation();
    // Bare Escape cancels recording without changing the stored value. Esc
    // *with* modifiers (Cmd+Esc, Ctrl+Esc) is a valid shortcut and is
    // committed by the capture path below — the cancel-on-Esc behaviour is
    // gated on "no modifiers held".
    if (
      event.key === 'Escape' &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey &&
      !event.shiftKey
    ) {
      stopRecording();
      return;
    }
    const captured = captureFromKeyboardEvent(event, target, platform);
    if (captured === null) {
      // Pure modifier press, or an unmapped code (IntlBackslash etc.). Keep
      // recording so the user can release-and-retry instead of having the
      // field silently bail out.
      return;
    }
    onChange(captured);
    stopRecording();
  };

  const handleBlur = (): void => {
    // Losing focus mid-recording aborts the capture so the binding doesn't
    // commit half-typed when the user clicks elsewhere in the form.
    if (recording) stopRecording();
  };

  const handleClear = (event: MouseEvent): void => {
    // Stop the click from bubbling to the parent button (which would
    // arm recording mode immediately after we cleared the value).
    event.stopPropagation();
    event.preventDefault();
    onChange('');
  };
</script>

<div class="hotkey-input">
  <button
    type="button"
    {id}
    bind:this={buttonEl}
    class="display"
    class:recording
    class:empty={!value}
    aria-label={recording ? recordingLabel : formatted || placeholder || ''}
    onclick={startRecording}
    onkeydown={handleKeyDown}
    onblur={handleBlur}
  >
    {#if recording}
      <span class="hint">{recordingLabel}</span>
    {:else if formatted}
      <span class="combo">{formatted}</span>
    {:else}
      <span class="hint">{placeholder ?? ''}</span>
    {/if}
  </button>
  {#if value && !recording}
    <button
      type="button"
      class="clear"
      aria-label={clearLabel}
      title={clearLabel}
      onclick={handleClear}
    >
      ×
    </button>
  {/if}
</div>

<style>
  .hotkey-input {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }
  .display {
    min-width: 9rem;
    padding: 0.3rem 0.6rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.18));
    border-radius: 6px;
    background: var(--bg-elevated, rgba(255, 255, 255, 0.04));
    color: var(--fg, #f5f5f5);
    font-family: inherit;
    font-size: 0.95rem;
    text-align: center;
    cursor: pointer;
  }
  .display:hover {
    border-color: var(--border-hover, rgba(255, 255, 255, 0.32));
  }
  .display:focus-visible {
    outline: 2px solid var(--accent, #6aa6ff);
    outline-offset: 1px;
  }
  .display.recording {
    border-color: var(--accent, #6aa6ff);
    background: var(--accent-soft, rgba(106, 166, 255, 0.16));
  }
  .display.empty .hint,
  .display .hint {
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-style: italic;
  }
  .combo {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    letter-spacing: 0.02em;
  }
  .clear {
    width: 1.5rem;
    height: 1.5rem;
    padding: 0;
    border: 1px solid transparent;
    border-radius: 4px;
    background: transparent;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    cursor: pointer;
    font-size: 1.1rem;
    line-height: 1;
  }
  .clear:hover {
    border-color: var(--border, rgba(255, 255, 255, 0.18));
    color: var(--fg, #f5f5f5);
  }
</style>

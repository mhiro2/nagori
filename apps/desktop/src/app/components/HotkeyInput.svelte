<script lang="ts">
  import {
    captureFromKeyboardEvent,
    formatAccelerator,
    type CaptureTarget,
  } from '../lib/keybindings';
  import type { Platform } from '../lib/types';

  // `simple`           — a single always-set field (the main global hotkey).
  //                      Shows the stored value or the placeholder, with a ×
  //                      to clear it. No default/override distinction.
  // `palette`          — an in-palette action that ships with a built-in
  //                      default. Empty = the default key (shown muted with a
  //                      "Default" marker); an override shows the custom key
  //                      with a ↺ "restore default" control.
  // `palette-optional` — an in-palette action with NO default (today only
  //                      `clear`). Empty = "Not set"; an override shows the
  //                      custom key with a × "remove shortcut" control.
  // `secondary`        — an optional global shortcut with no default. Empty =
  //                      "Not set" + a "Disabled" marker; an override shows the
  //                      custom key with a × "disable" control.
  type HotkeyVariant = 'simple' | 'palette' | 'palette-optional' | 'secondary';

  type Props = {
    value: string;
    platform: Platform | undefined;
    target: CaptureTarget;
    variant?: HotkeyVariant;
    // Built-in default for this action, already formatted for the platform.
    // Only consulted for the `palette` variant; null/undefined elsewhere.
    defaultDisplay?: string | null;
    // Accessible name for the field, with the action folded in (e.g.
    // "Toggle pin shortcut"). The current key/state is appended for AT.
    // Omitted for the `simple` variant, which keeps the value-only name.
    ariaLabel?: string;
    placeholder?: string;
    recordingLabel: string;
    recordingCancelHint?: string;
    // The × control on the `simple` variant (the global hotkey field).
    clearLabel: string;
    // Group-variant state strings — see the variant doc above.
    defaultMarker?: string;
    disabledMarker?: string;
    notSet?: string;
    // Visible text on the restore control (e.g. "Reset" / 「リセット」). An action
    // word — deliberately NOT `defaultMarker`, so the in-field "Default" status
    // and the restore *action* never read as the same thing.
    restoreText?: string;
    // Trailing-control accessible names, pre-composed with the action.
    // `restoreLabel` drives the restore chip (palette); `removeLabel` drives the
    // × control (palette-optional → "remove", secondary → "disable").
    restoreLabel?: string;
    removeLabel?: string;
    onChange: (next: string) => void;
    // Optional id for label association — the surrounding row labels the
    // input via `aria-label`, but an explicit `id` still helps AT pair a
    // visible label with the composite control.
    id?: string;
  };

  let {
    value,
    platform,
    target,
    variant = 'simple',
    defaultDisplay,
    ariaLabel,
    placeholder,
    recordingLabel,
    recordingCancelHint,
    clearLabel,
    defaultMarker,
    disabledMarker,
    notSet,
    restoreText,
    restoreLabel,
    removeLabel,
    onChange,
    id,
  }: Props = $props();

  let recording = $state(false);
  let buttonEl: HTMLButtonElement | undefined = $state();

  const hasOverride = $derived(!!value);
  const overrideFormatted = $derived(value ? formatAccelerator(value, platform) : '');

  // Showing a real key glyph (vs. a placeholder / "Not set" hint): either the
  // user's override, or — for the `palette` variant only — the built-in
  // default rendered muted.
  const showingKey = $derived(hasOverride || (variant === 'palette' && !!defaultDisplay));
  const keyText = $derived(hasOverride ? overrideFormatted : (defaultDisplay ?? ''));
  // The built-in default is shown muted so it never reads as a custom binding.
  const defaultShown = $derived(showingKey && !hasOverride);
  const placeholderText = $derived(variant === 'simple' ? (placeholder ?? '') : (notSet ?? ''));

  // Muted state marker rendered alongside the value (text, never colour-only):
  // "Default" for an un-customized palette action, "Disabled" for an unset
  // secondary shortcut. Empty once the user supplies an override.
  const stateMarker = $derived.by(() => {
    if (hasOverride) return '';
    if (variant === 'palette' && defaultDisplay) return defaultMarker ?? '';
    if (variant === 'secondary') return disabledMarker ?? '';
    return '';
  });

  // Trailing control kind. `clear`/`remove` both render × but carry different
  // accessible names; `restore` renders ↺. All three call `onChange('')` —
  // they only differ in what that means to the user, so the label/icon split
  // is what communicates intent (a11y: not colour-dependent).
  type Trailing = 'clear' | 'restore' | 'remove' | null;
  const trailing = $derived.by<Trailing>(() => {
    if (recording) return null;
    if (variant === 'simple') return hasOverride ? 'clear' : null;
    if (!hasOverride) return null;
    return variant === 'palette' ? 'restore' : 'remove';
  });
  const trailingLabel = $derived(
    trailing === 'restore'
      ? (restoreLabel ?? '')
      : trailing === 'remove'
        ? (removeLabel ?? '')
        : clearLabel,
  );
  // The restore control is a short text chip — an action word ("Reset" /
  // 「リセット」), deliberately distinct from the in-field "Default" status
  // marker so the two never read as the same thing, and clearer than a ↺ glyph
  // (which reads as "refresh/reload"). Remove/disable stays a plain × icon.
  // (aria-label/title still carry the full action-scoped meaning for AT.)
  const trailingIsText = $derived(trailing === 'restore');
  const trailingContent = $derived(trailing === 'restore' ? (restoreText ?? '') : '×');

  // Accessible name for the recording button. While recording, announce the
  // prompt (+ the cancel affordance). Otherwise fold the current value/state
  // into the action-scoped name so AT users hear "Toggle pin shortcut, ⌘P,
  // Default" rather than a bare glyph. The `simple` variant has no action
  // name, so it keeps the historical value-or-placeholder name.
  const buttonAriaLabel = $derived.by(() => {
    if (recording) {
      return recordingCancelHint ? `${recordingLabel} ${recordingCancelHint}` : recordingLabel;
    }
    if (!ariaLabel) return overrideFormatted || placeholder || '';
    const state = showingKey ? keyText : placeholderText;
    const parts = [ariaLabel, state, stateMarker].filter((p) => p.length > 0);
    return parts.join(', ');
  });

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
    // Clearing the override removes this very (trailing) button from the DOM,
    // which would otherwise drop keyboard focus to <body>. Hand focus back to
    // the row's main recording button so keyboard/AT users stay oriented.
    buttonEl?.focus();
  };
</script>

<div class="hotkey-input">
  <button
    type="button"
    {id}
    bind:this={buttonEl}
    class="display"
    class:recording
    class:empty={!showingKey}
    aria-label={buttonAriaLabel}
    onclick={startRecording}
    onkeydown={handleKeyDown}
    onblur={handleBlur}
  >
    {#if recording}
      <span class="hint">{recordingLabel}</span>
      {#if recordingCancelHint}
        <span class="cancel-hint">{recordingCancelHint}</span>
      {/if}
    {:else if showingKey}
      <span class="combo" class:muted={defaultShown}>{keyText}</span>
      {#if stateMarker}
        <span class="marker">{stateMarker}</span>
      {/if}
    {:else}
      <span class="hint">{placeholderText}</span>
      {#if stateMarker}
        <span class="marker">{stateMarker}</span>
      {/if}
    {/if}
  </button>
  {#if trailing}
    <button
      type="button"
      class="clear"
      class:text-chip={trailingIsText}
      aria-label={trailingLabel}
      title={trailingLabel}
      onclick={handleClear}
    >
      {trailingContent}
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
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    min-width: 11rem;
    padding: 0.3rem 0.6rem;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.18));
    border-radius: 6px;
    background: var(--bg-elevated, rgba(255, 255, 255, 0.04));
    color: var(--fg, #f5f5f5);
    font-family: inherit;
    font-size: 0.95rem;
    text-align: left;
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
    background: var(--bg-accent-soft, rgba(106, 166, 255, 0.16));
  }
  .display .hint {
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-style: italic;
  }
  .cancel-hint {
    margin-left: auto;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-size: 0.75rem;
    font-style: normal;
  }
  .combo {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    letter-spacing: 0.02em;
  }
  /* The built-in default is dimmed so it never reads as a custom binding. */
  .combo.muted {
    color: var(--muted, rgba(255, 255, 255, 0.55));
  }
  /* Muted text marker ("Default" / "Disabled") pushed to the trailing edge —
     mirrors the `.kind-badge` idiom (muted, small, tracked) rather than a
     pill, and stays legible without relying on colour alone. */
  .marker {
    margin-left: auto;
    color: var(--muted, rgba(255, 255, 255, 0.55));
    font-size: 0.7rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
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
  /* The restore control reads as a short text chip rather than the square ×
     icon: auto width so the localized "Reset" / 「リセット」 / "Zurücksetzen"
     fits, and a persistent rounded border so it reads as clickable (the ×
     only borders on hover). Sentence case (no uppercase) keeps the longer
     locale words legible. */
  .clear.text-chip {
    width: auto;
    height: auto;
    padding: 0.12rem 0.55rem;
    border-color: var(--border, rgba(255, 255, 255, 0.18));
    border-radius: 999px;
    font-size: 0.75rem;
    white-space: nowrap;
  }
</style>

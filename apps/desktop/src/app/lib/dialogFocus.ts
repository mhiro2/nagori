// Keyboard containment for the palette's modal confirm dialogs. The palette
// resolves its shortcuts on a window-level keydown listener, so a dialog has
// to keep both its keystrokes and its focus to itself: a key that bubbles out
// (or lands on an element behind the overlay) would paste, delete or pin the
// entry the dialog is asking about.

const FOCUSABLE = [
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  'a[href]',
  '[tabindex]:not([tabindex="-1"])',
].join(',');

// Wraps Tab / Shift+Tab around the dialog's own controls so focus never walks
// out onto the search box or the result list behind the overlay. When every
// control is disabled (a request in flight) focus parks on the dialog itself.
export const trapTabFocus = (event: KeyboardEvent, dialog: HTMLElement): void => {
  if (event.key !== 'Tab') return;
  const items = Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE));
  const first = items[0];
  const last = items.at(-1);
  if (!first || !last) {
    event.preventDefault();
    dialog.focus();
    return;
  }
  const active = document.activeElement;
  if (event.shiftKey) {
    if (active === first || active === dialog || !dialog.contains(active)) {
      event.preventDefault();
      last.focus();
    }
  } else if (active === last || !dialog.contains(active)) {
    event.preventDefault();
    first.focus();
  }
};

// Moves focus into the dialog on mount and hands it back to whatever held it
// before (normally the search box) on unmount, so the palette's keyboard flow
// resumes where the user left it instead of on `<body>`. Returns the cleanup.
export const holdDialogFocus = (dialog: HTMLElement): (() => void) => {
  const previous = document.activeElement;
  dialog.focus();
  return () => {
    if (previous instanceof HTMLElement && previous.isConnected) {
      previous.focus();
    }
  };
};

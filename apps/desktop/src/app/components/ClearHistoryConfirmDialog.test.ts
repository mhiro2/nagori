import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/commands', async () => {
  const { commandsMock } = await import('../test-helpers/moduleMocks');
  return commandsMock({
    clearHistory: vi.fn(async () => 3),
    setConfirmClearHistory: vi.fn(async () => undefined),
  });
});

import { clearHistory, setConfirmClearHistory } from '../lib/commands';
import ClearHistoryConfirmDialog from './ClearHistoryConfirmDialog.svelte';

const labels = {
  title: 'Clear history?',
  description: 'Every unpinned item is deleted.',
  undoWarning: "You can't undo this.",
  dontAskAgain: "Don't ask again",
  cancel: 'Cancel',
  confirm: 'Clear',
  failed: 'Could not clear the history.',
};

const mount = () => {
  const onCleared = vi.fn();
  const onClose = vi.fn();
  const view = render(ClearHistoryConfirmDialog, { labels, onCleared, onClose });
  return { ...view, onCleared, onClose };
};

describe('ClearHistoryConfirmDialog', () => {
  beforeEach(() => {
    vi.mocked(clearHistory).mockResolvedValue(3);
    vi.mocked(setConfirmClearHistory).mockResolvedValue(
      undefined as unknown as Awaited<ReturnType<typeof setConfirmClearHistory>>,
    );
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('clears and notifies the caller on confirm', async () => {
    const { getByTestId, onCleared, onClose } = mount();
    await userEvent.click(getByTestId('clear-history-confirm-clear'));
    expect(clearHistory).toHaveBeenCalledTimes(1);
    expect(onCleared).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('does not touch the history on cancel', async () => {
    const { getByTestId, onCleared, onClose } = mount();
    await userEvent.click(getByTestId('clear-history-confirm-cancel'));
    expect(clearHistory).not.toHaveBeenCalled();
    expect(onCleared).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('records the suppression only when the box is ticked', async () => {
    const { getByTestId } = mount();
    await userEvent.click(getByTestId('clear-history-confirm-clear'));
    expect(setConfirmClearHistory).not.toHaveBeenCalled();

    cleanup();
    const second = mount();
    await userEvent.click(second.getByTestId('clear-history-confirm-suppress'));
    await userEvent.click(second.getByTestId('clear-history-confirm-clear'));
    expect(setConfirmClearHistory).toHaveBeenCalledWith(false);
  });

  // A failed clear must keep the dialog open with the reason on screen: closing
  // it would read as "cleared" while the history is still there.
  it('keeps the dialog open and surfaces the error when the clear fails', async () => {
    vi.mocked(clearHistory).mockRejectedValue(new Error('nope'));
    const { getByTestId, getByRole, onCleared, onClose } = mount();
    await userEvent.click(getByTestId('clear-history-confirm-clear'));
    expect(onCleared).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(getByRole('alert').textContent).toBeTruthy();
  });

  // The suppression is the user's answer to "stop asking me", independent of
  // whether this particular clear worked — so a failure to persist it must not
  // block the clear.
  it('still clears when recording the suppression fails', async () => {
    vi.mocked(setConfirmClearHistory).mockRejectedValue(new Error('nope'));
    const { getByTestId, onCleared } = mount();
    await userEvent.click(getByTestId('clear-history-confirm-suppress'));
    await userEvent.click(getByTestId('clear-history-confirm-clear'));
    expect(clearHistory).toHaveBeenCalledTimes(1);
    expect(onCleared).toHaveBeenCalledTimes(1);
  });
});

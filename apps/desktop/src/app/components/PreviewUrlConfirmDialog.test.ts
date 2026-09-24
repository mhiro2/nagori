import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/commands', async () => {
  const { commandsMock } = await import('../test-helpers/moduleMocks');
  return commandsMock({ openUrlExternal: vi.fn(async () => undefined) });
});

import { openUrlExternal } from '../lib/commands';
import PreviewUrlConfirmDialog from './PreviewUrlConfirmDialog.svelte';

const labels = {
  title: 'Open link?',
  description: ({ host }: { host: string }) => `Open ${host}?`,
  cancel: 'Cancel',
  confirm: 'Open',
  openFailed: 'Could not open the link.',
};

const mount = () => {
  const onClose = vi.fn();
  const view = render(PreviewUrlConfirmDialog, {
    entryId: 'entry-1',
    body: { type: 'url', url: 'https://example.com/', hostDisplay: 'example.com' },
    labels,
    onClose,
  });
  return { ...view, onClose };
};

// The palette resolves its shortcuts on a window keydown listener, so a key
// that bubbles out of the dialog would act on the entry behind it.
describe('PreviewUrlConfirmDialog keyboard containment', () => {
  const windowKeys: string[] = [];
  const onWindowKeydown = (e: KeyboardEvent): void => {
    windowKeys.push(e.key);
  };

  beforeEach(() => {
    windowKeys.length = 0;
    window.addEventListener('keydown', onWindowKeydown);
  });

  afterEach(() => {
    window.removeEventListener('keydown', onWindowKeydown);
    cleanup();
  });

  it('cancels on Enter over Cancel without the key reaching the window', async () => {
    const { getByTestId, onClose } = mount();
    getByTestId('preview-url-confirm-cancel').focus();
    await userEvent.keyboard('{Enter}');
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(openUrlExternal).not.toHaveBeenCalled();
    expect(windowKeys).toEqual([]);
  });

  it('keeps palette chords and Escape inside the dialog', async () => {
    const { onClose } = mount();
    await userEvent.keyboard('{Meta>}{Backspace}p{/Meta}');
    expect(windowKeys).toEqual([]);
    await userEvent.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(windowKeys).toEqual([]);
  });

  it('wraps Tab around the dialog buttons', async () => {
    const { getByTestId } = mount();
    const cancel = getByTestId('preview-url-confirm-cancel');
    const open = getByTestId('preview-url-confirm-open');
    open.focus();
    await userEvent.tab();
    expect(document.activeElement).toBe(cancel);
    await userEvent.tab({ shift: true });
    expect(document.activeElement).toBe(open);
  });
});

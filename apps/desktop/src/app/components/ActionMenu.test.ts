import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  runAiAction: vi.fn(),
  saveAiResult: vi.fn(),
}));

import { runAiAction, saveAiResult } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { SearchResultDto } from '../lib/types';
import ActionMenu from './ActionMenu.svelte';

const sample = (overrides: Partial<SearchResultDto> = {}): SearchResultDto => ({
  id: 'entry-id',
  kind: 'text',
  preview: 'value',
  score: 0,
  createdAt: '2026-05-05T00:00:00Z',
  pinned: false,
  sensitivity: 'Public',
  rankReasons: [],
  ...overrides,
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
});

afterEach(cleanup);

describe('ActionMenu', () => {
  it('renders nothing when open is false', () => {
    const { container } = render(ActionMenu, {
      props: { open: false, target: sample(), onClose: () => {} },
    });
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it('renders a dialog with all AI actions when open', () => {
    const { getByRole, getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(getByRole('dialog')).toBeTruthy();
    expect(getByText('Summarize')).toBeTruthy();
    expect(getByText('Translate')).toBeTruthy();
    expect(getByText('Redact secrets')).toBeTruthy();
  });

  it('invokes onClose when the close button is clicked', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByRole, container } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose },
    });
    // The close glyph is rendered as ×; grab it via the button class.
    const closeBtn = container.querySelector('.close');
    expect(closeBtn).toBeTruthy();
    expect(getByRole('dialog')).toBeTruthy();
    await user.click(closeBtn as HTMLElement);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('invokes onClose when the scrim is clicked', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { container } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose },
    });
    const scrim = container.querySelector('.scrim');
    expect(scrim).toBeTruthy();
    await user.click(scrim as HTMLElement);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('does not close when the inner menu is clicked (event stopped)', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByRole } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose },
    });
    await user.click(getByRole('dialog'));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('dispatches runAiAction with the target id and renders the result', async () => {
    const user = userEvent.setup();
    vi.mocked(runAiAction).mockResolvedValue({
      text: 'summary text',
      warnings: [],
    });

    const { getByText, findByText } = render(ActionMenu, {
      props: { open: true, target: sample({ id: 'abc' }), onClose: () => {} },
    });
    await user.click(getByText('Summarize'));
    expect(runAiAction).toHaveBeenCalledWith('Summarize', 'abc');

    expect(await findByText('summary text')).toBeTruthy();
  });

  it('surfaces a runFailed message when the command rejects', async () => {
    const user = userEvent.setup();
    vi.mocked(runAiAction).mockRejectedValue(new Error('provider down'));

    const { getByText, findByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByText('Translate'));
    expect(await findByText('provider down')).toBeTruthy();
  });

  it('disables every action button while a request is in flight', async () => {
    const user = userEvent.setup();
    let resolve: ((value: { text: string; warnings: string[] }) => void) | undefined;
    vi.mocked(runAiAction).mockReturnValue(
      new Promise((r) => {
        resolve = r;
      }),
    );

    const { getByText, getAllByRole } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByText('Summarize'));
    // The eight AI-action buttons should all be disabled while pending.
    const actionButtons = getAllByRole('button').filter(
      (btn) => btn.parentElement?.tagName === 'LI',
    );
    expect(actionButtons).toHaveLength(8);
    for (const btn of actionButtons) {
      expect((btn as HTMLButtonElement).disabled).toBe(true);
    }

    resolve?.({ text: 'done', warnings: [] });
  });

  it('shows the tauriRequired hint when the runtime is unavailable', () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const { getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(getByText('AI actions require the Tauri runtime.')).toBeTruthy();
  });

  it('auto-focuses the dialog when opened', async () => {
    const { getByRole } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    const dialog = getByRole('dialog');
    // The dialog has tabindex=-1 and the component focuses it via $effect
    // so screen readers announce the role and Escape from inside has a
    // reachable target.
    expect(document.activeElement).toBe(dialog);
  });

  it('invokes onClose when Escape is pressed inside the dialog', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByRole } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose },
    });
    // The component auto-focuses the dialog on open, so keyboard events
    // are delivered there. The dialog stops keydown propagation so the
    // scrim's Escape handler never fires; the dialog itself must close.
    expect(document.activeElement).toBe(getByRole('dialog'));
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('forwards the result body to saveAiResult on save', async () => {
    const user = userEvent.setup();
    vi.mocked(runAiAction).mockResolvedValue({ text: 'result body', warnings: [] });
    vi.mocked(saveAiResult).mockResolvedValue({
      id: 'saved-1',
      kind: 'text',
      preview: 'result body',
      createdAt: '2026-05-05T00:00:00Z',
      updatedAt: '2026-05-05T00:00:00Z',
      useCount: 0,
      pinned: false,
      sensitivity: 'Public',
    });

    const { findByText, getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByText('Summarize'));
    await findByText('result body');
    await user.click(getByText('Save as new entry'));
    expect(saveAiResult).toHaveBeenCalledWith('result body');
  });
});

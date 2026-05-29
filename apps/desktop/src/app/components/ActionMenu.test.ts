import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AiAvailability } from '../lib/types';

// `vi.mock` is hoisted above module-level consts, so the shared mock state has
// to be defined via `vi.hoisted` to be reachable from the factory.
const { handlers, AI_EVENTS } = vi.hoisted(() => ({
  // Captured `nagori://ai/*` handlers so tests can drive the streaming flow.
  handlers: {} as Record<string, (payload: unknown) => void>,
  AI_EVENTS: {
    aiStarted: 'nagori://ai/started',
    aiDelta: 'nagori://ai/delta',
    aiReplace: 'nagori://ai/replace',
    aiDone: 'nagori://ai/done',
    aiError: 'nagori://ai/error',
    aiCancelled: 'nagori://ai/cancelled',
  },
}));

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
  TAURI_EVENTS: AI_EVENTS,
  subscribe: vi.fn((event: string, handler: (payload: unknown) => void) => {
    handlers[event] = handler;
    return () => {
      delete handlers[event];
    };
  }),
}));

vi.mock('../lib/commands', () => ({
  runQuickAction: vi.fn(),
  startAiAction: vi.fn(),
  cancelAiAction: vi.fn(),
  getAiAvailability: vi.fn(),
  saveAiResult: vi.fn(),
}));

import {
  cancelAiAction,
  getAiAvailability,
  runQuickAction,
  saveAiResult,
  startAiAction,
} from '../lib/commands';
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
  representationSummary: [],
  ...overrides,
});

const availability = (summarizeAvailable: boolean): AiAvailability => ({
  provider: summarizeAvailable ? 'appleNative' : 'disabled',
  overallStatus: summarizeAvailable ? 'available' : 'disabled',
  actions: [
    {
      action: 'Summarize',
      status: summarizeAvailable ? 'available' : 'disabled_by_settings',
      available: summarizeAvailable,
      ...(summarizeAvailable
        ? {}
        : { remediation: 'ai.unavailable.apple_intelligence_not_enabled' }),
    },
  ],
});

beforeEach(() => {
  vi.clearAllMocks();
  for (const key of Object.keys(handlers)) delete handlers[key];
  vi.mocked(isTauri).mockReturnValue(true);
  vi.mocked(getAiAvailability).mockResolvedValue(availability(true));
});

afterEach(cleanup);

const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

describe('ActionMenu', () => {
  it('renders nothing when open is false', () => {
    const { container } = render(ActionMenu, {
      props: { open: false, target: sample(), onClose: () => {} },
    });
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it('renders a dialog with the quick actions plus the AI entry when open', () => {
    const { getByRole, getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(getByRole('dialog')).toBeTruthy();
    expect(getByText('Summarize (first sentence)')).toBeTruthy();
    expect(getByText('Format JSON')).toBeTruthy();
    expect(getByText('Extract tasks')).toBeTruthy();
    expect(getByText('Redact secrets')).toBeTruthy();
    expect(getByText('AI: Summarize')).toBeTruthy();
  });

  it('invokes onClose when the close button is clicked', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByRole, container } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose },
    });
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

  it('dispatches runQuickAction with the target id and renders the result', async () => {
    const user = userEvent.setup();
    vi.mocked(runQuickAction).mockResolvedValue({
      text: 'first sentence.',
      warnings: [],
    });

    const { getByText, findByText } = render(ActionMenu, {
      props: { open: true, target: sample({ id: 'abc' }), onClose: () => {} },
    });
    await user.click(getByText('Summarize (first sentence)'));
    expect(runQuickAction).toHaveBeenCalledWith('SummarizeFirstSentence', 'abc');
    expect(await findByText('first sentence.')).toBeTruthy();
  });

  it('surfaces a runFailed message when a quick action rejects', async () => {
    const user = userEvent.setup();
    vi.mocked(runQuickAction).mockRejectedValue(new Error('bad json'));

    const { getByText, findByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByText('Format JSON'));
    expect(await findByText('bad json')).toBeTruthy();
  });

  it('disables every quick-action button while a request is in flight', async () => {
    const user = userEvent.setup();
    let resolve: ((value: { text: string; warnings: string[] }) => void) | undefined;
    vi.mocked(runQuickAction).mockReturnValue(
      new Promise((r) => {
        resolve = r;
      }),
    );

    const { getByText, getAllByRole } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByText('Format JSON'));
    const actionButtons = getAllByRole('button').filter(
      (btn) => btn.parentElement?.tagName === 'LI',
    );
    expect(actionButtons).toHaveLength(4);
    for (const btn of actionButtons) {
      expect((btn as HTMLButtonElement).disabled).toBe(true);
    }
    resolve?.({ text: 'done', warnings: [] });
  });

  it('streams an AI summary via the request-scoped events', async () => {
    const user = userEvent.setup();
    vi.mocked(startAiAction).mockResolvedValue('req-1');

    const { getByText, findByText } = render(ActionMenu, {
      props: { open: true, target: sample({ id: 'abc' }), onClose: () => {} },
    });
    await flush(); // let the availability probe resolve
    await user.click(getByText('AI: Summarize'));
    expect(startAiAction).toHaveBeenCalledWith('Summarize', 'abc');
    await flush(); // let startAiAction resolve so aiRequestId is set

    handlers[AI_EVENTS.aiDelta]?.({ requestId: 'req-1', seq: 0, text: 'Hello ' });
    handlers[AI_EVENTS.aiDone]?.({
      requestId: 'req-1',
      finalText: 'Hello world',
      warnings: [],
    });
    expect(await findByText('Hello world')).toBeTruthy();
  });

  it('disables the AI button with a reason when summarize is unavailable', async () => {
    vi.mocked(getAiAvailability).mockResolvedValue(availability(false));
    const { getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await flush();
    const aiButton = getByText('AI: Summarize').closest('button') as HTMLButtonElement;
    expect(aiButton.disabled).toBe(true);
    // The remediation hint for the disabled state is surfaced.
    expect(
      getByText('Enable Apple Intelligence in System Settings to use AI actions.'),
    ).toBeTruthy();
  });

  it('cancels the in-flight AI run', async () => {
    const user = userEvent.setup();
    vi.mocked(startAiAction).mockResolvedValue('req-9');

    const { getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await flush();
    await user.click(getByText('AI: Summarize'));
    await flush();
    await user.click(getByText('Cancel'));
    expect(cancelAiAction).toHaveBeenCalledWith('req-9');
  });

  it('shows the tauriRequired hint when the runtime is unavailable', () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const { getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(getByText('Quick actions require the Tauri runtime.')).toBeTruthy();
  });

  it('auto-focuses the dialog when opened', () => {
    const { getByRole } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(document.activeElement).toBe(getByRole('dialog'));
  });

  it('invokes onClose when Escape is pressed inside the dialog', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByRole } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose },
    });
    expect(document.activeElement).toBe(getByRole('dialog'));
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('forwards the result body to saveAiResult on save', async () => {
    const user = userEvent.setup();
    vi.mocked(runQuickAction).mockResolvedValue({ text: 'result body', warnings: [] });
    vi.mocked(saveAiResult).mockResolvedValue({
      id: 'saved-1',
      kind: 'text',
      preview: 'result body',
      createdAt: '2026-05-05T00:00:00Z',
      updatedAt: '2026-05-05T00:00:00Z',
      useCount: 0,
      pinned: false,
      sensitivity: 'Public',
      representationSummary: [],
    });

    const { findByText, getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByText('Format JSON'));
    await findByText('result body');
    await user.click(getByText('Save as new entry'));
    expect(saveAiResult).toHaveBeenCalledWith('result body');
  });

  it('omits the clear-all section when onClearAll is not wired', () => {
    const { queryByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(queryByText('Clear all history')).toBeNull();
  });

  it('clears all history and closes when the clear-all button is clicked', async () => {
    const user = userEvent.setup();
    const onClearAll = vi.fn();
    const onClose = vi.fn();
    const { getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClearAll, onClose },
    });
    await user.click(getByText('Clear all history'));
    expect(onClearAll).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('disables the clear-all button outside the Tauri runtime', () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const { getByText } = render(ActionMenu, {
      props: { open: true, target: sample(), onClearAll: vi.fn(), onClose: () => {} },
    });
    expect((getByText('Clear all history') as HTMLButtonElement).disabled).toBe(true);
  });
});

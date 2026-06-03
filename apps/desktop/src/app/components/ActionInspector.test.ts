import { cleanup, render, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AiActionId, AiAvailability } from '../lib/types';

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

// AI surfaces gate on a wired backend. Default to supported (macOS) so the
// AI-action tests render the buttons; the gating test flips it to false.
vi.mock('../stores/capabilities.svelte', () => ({
  aiActionsSupported: vi.fn(() => true),
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
import { aiActionsSupported } from '../stores/capabilities.svelte';
import ActionInspector from './ActionInspector.svelte';

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

// The streaming text actions the inspector surfaces (Translate is CLI-only).
const TEXT_ACTIONS: AiActionId[] = [
  'Summarize',
  'Rewrite',
  'FormatMarkdown',
  'ExtractTasks',
  'ExplainCode',
];

const availability = (actionsAvailable: boolean): AiAvailability => ({
  provider: actionsAvailable ? 'appleNative' : 'disabled',
  overallStatus: actionsAvailable ? 'available' : 'disabled',
  actions: TEXT_ACTIONS.map((action) => {
    const entry: AiAvailability['actions'][number] = {
      action,
      status: actionsAvailable ? 'available' : 'disabled_by_settings',
      available: actionsAvailable,
    };
    if (!actionsAvailable) {
      entry.remediation = 'ai.unavailable.apple_intelligence_not_enabled';
    }
    return entry;
  }),
});

beforeEach(() => {
  vi.clearAllMocks();
  for (const key of Object.keys(handlers)) delete handlers[key];
  vi.mocked(isTauri).mockReturnValue(true);
  vi.mocked(getAiAvailability).mockResolvedValue(availability(true));
  vi.mocked(aiActionsSupported).mockReturnValue(true);
});

afterEach(cleanup);

const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

describe('ActionInspector', () => {
  it('renders nothing when open is false', () => {
    const { container } = render(ActionInspector, {
      props: { open: false, target: sample(), onClose: () => {} },
    });
    expect(container.querySelector('[data-testid="action-inspector"]')).toBeNull();
  });

  it('renders the docked panel with the deterministic and AI actions in one list', () => {
    const { getByTestId, getByText } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(getByTestId('action-inspector')).toBeTruthy();
    // Deterministic actions.
    expect(getByText('Summarize (first sentence)')).toBeTruthy();
    expect(getByText('Format JSON')).toBeTruthy();
    expect(getByText('Extract tasks')).toBeTruthy();
    expect(getByText('Redact secrets')).toBeTruthy();
    // AI actions: no `AI:` prefix; surfaced with a badge instead. The AI
    // "extract tasks" reads as "Organize tasks" so it no longer collides with
    // the deterministic entry.
    expect(getByText('Summarize')).toBeTruthy();
    expect(getByText('Rewrite')).toBeTruthy();
    expect(getByText('Format as Markdown')).toBeTruthy();
    expect(getByText('Organize tasks')).toBeTruthy();
    expect(getByText('Explain code')).toBeTruthy();
  });

  it('hides AI actions (and the unavailable footnote) when no backend is wired', async () => {
    vi.mocked(aiActionsSupported).mockReturnValue(false);
    const { getByTestId, queryByTestId, queryByText } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await flush(); // let any availability effect settle
    // Quick actions still render.
    expect(getByTestId('quick-FormatJson')).toBeTruthy();
    // No AI action buttons.
    for (const action of TEXT_ACTIONS) {
      expect(queryByTestId(`ai-${action}`)).toBeNull();
    }
    // The "AI unavailable" footnote is suppressed, not just empty.
    expect(queryByText('AI actions are unavailable right now.')).toBeNull();
    // And the probe is never even issued.
    expect(getAiAvailability).not.toHaveBeenCalled();
  });

  it('shows the target summary for the selected entry', () => {
    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample({ preview: 'hello there' }), onClose: () => {} },
    });
    const target = getByTestId('action-target');
    expect(within(target).getByText('hello there')).toBeTruthy();
  });

  it('invokes onClose when the close button is clicked', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByTestId, container } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose },
    });
    const closeBtn = container.querySelector('.close');
    expect(closeBtn).toBeTruthy();
    expect(getByTestId('action-inspector')).toBeTruthy();
    await user.click(closeBtn as HTMLElement);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('dispatches runQuickAction with the target id and renders the result', async () => {
    const user = userEvent.setup();
    vi.mocked(runQuickAction).mockResolvedValue({
      text: 'first sentence.',
      warnings: [],
    });

    const { getByTestId, findByText } = render(ActionInspector, {
      props: { open: true, target: sample({ id: 'abc' }), onClose: () => {} },
    });
    await user.click(getByTestId('quick-SummarizeFirstSentence'));
    expect(runQuickAction).toHaveBeenCalledWith('SummarizeFirstSentence', 'abc');
    expect(await findByText('first sentence.')).toBeTruthy();
  });

  it('surfaces the error message when a quick action rejects', async () => {
    const user = userEvent.setup();
    vi.mocked(runQuickAction).mockRejectedValue(new Error('bad json'));

    const { getByTestId, findByText } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByTestId('quick-FormatJson'));
    expect(await findByText('bad json')).toBeTruthy();
  });

  it('disables every action button while a request is in flight', async () => {
    const user = userEvent.setup();
    let resolve: ((value: { text: string; warnings: string[] }) => void) | undefined;
    vi.mocked(runQuickAction).mockReturnValue(
      new Promise((r) => {
        resolve = r;
      }),
    );

    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByTestId('quick-FormatJson'));
    const picker = getByTestId('action-picker');
    const actionButtons = within(picker).getAllByRole('button');
    expect(actionButtons.length).toBeGreaterThan(0);
    for (const btn of actionButtons) {
      expect((btn as HTMLButtonElement).disabled).toBe(true);
    }
    resolve?.({ text: 'done', warnings: [] });
  });

  it('streams an AI summary via the request-scoped events', async () => {
    const user = userEvent.setup();
    vi.mocked(startAiAction).mockResolvedValue('req-1');

    const { getByTestId, findByText } = render(ActionInspector, {
      props: { open: true, target: sample({ id: 'abc' }), onClose: () => {} },
    });
    await flush(); // let the availability probe resolve
    await user.click(getByTestId('ai-Summarize'));
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

  it('starts a non-summarize AI action with its own id', async () => {
    const user = userEvent.setup();
    vi.mocked(startAiAction).mockResolvedValue('req-2');

    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample({ id: 'xyz' }), onClose: () => {} },
    });
    await flush(); // let the availability probe resolve
    await user.click(getByTestId('ai-Rewrite'));
    expect(startAiAction).toHaveBeenCalledWith('Rewrite', 'xyz');
  });

  it('disables the AI button with a reason when it is unavailable', async () => {
    vi.mocked(getAiAvailability).mockResolvedValue(availability(false));
    const { getByTestId, getByText } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await flush();
    expect((getByTestId('ai-Summarize') as HTMLButtonElement).disabled).toBe(true);
    // The remediation hint for the disabled state is surfaced.
    expect(
      getByText('Enable Apple Intelligence in System Settings to use AI actions.'),
    ).toBeTruthy();
  });

  it('disables every action with a reason for an image target', async () => {
    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample({ kind: 'image' }), onClose: () => {} },
    });
    await flush();
    const picker = getByTestId('action-picker');
    const buttons = within(picker).getAllByRole('button');
    expect(buttons.length).toBeGreaterThan(0);
    for (const btn of buttons) {
      expect((btn as HTMLButtonElement).disabled).toBe(true);
      expect(btn.getAttribute('title')).toBe("Actions don't apply to images.");
    }
  });

  it('suppresses the AI-unavailable footnote when the kind gates every AI action off', async () => {
    // AI is unavailable *and* the target is an image: the per-button reason
    // already explains the image case, so the AI-availability footnote (which
    // would say "enable Apple Intelligence") must not also appear.
    vi.mocked(getAiAvailability).mockResolvedValue(availability(false));
    const { queryByText } = render(ActionInspector, {
      props: { open: true, target: sample({ kind: 'image' }), onClose: () => {} },
    });
    await flush();
    expect(
      queryByText('Enable Apple Intelligence in System Settings to use AI actions.'),
    ).toBeNull();
    expect(queryByText('AI actions are unavailable right now.')).toBeNull();
  });

  it('leaves only Redact secrets runnable for a URL target', async () => {
    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample({ kind: 'url' }), onClose: () => {} },
    });
    await flush();
    // Redacting a token-bearing URL is meaningful, so that one stays enabled.
    expect((getByTestId('quick-RedactSecrets') as HTMLButtonElement).disabled).toBe(false);
    // The text transforms would mangle a bare URL, so they are gated off.
    const gated = getByTestId('quick-FormatJson') as HTMLButtonElement;
    expect(gated.disabled).toBe(true);
    expect(gated.getAttribute('title')).toBe("This action doesn't apply to URLs.");
    for (const action of TEXT_ACTIONS) {
      expect((getByTestId(`ai-${action}`) as HTMLButtonElement).disabled).toBe(true);
    }
  });

  it('cancels the in-flight AI run from the work area', async () => {
    const user = userEvent.setup();
    vi.mocked(startAiAction).mockResolvedValue('req-9');

    const { getByTestId, getByText } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await flush();
    await user.click(getByTestId('ai-Summarize'));
    await flush();
    await user.click(getByText('Cancel'));
    expect(cancelAiAction).toHaveBeenCalledWith('req-9');
  });

  it('cancels an in-flight stream on Escape instead of closing', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    vi.mocked(startAiAction).mockResolvedValue('req-esc');

    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose },
    });
    await flush();
    await user.click(getByTestId('ai-Summarize'));
    await flush();
    await user.keyboard('{Escape}');
    expect(cancelAiAction).toHaveBeenCalledWith('req-esc');
    expect(onClose).not.toHaveBeenCalled();
  });

  it('cancels an AI run requested during startup once the id arrives', async () => {
    const user = userEvent.setup();
    let resolveStart: ((id: string) => void) | undefined;
    vi.mocked(startAiAction).mockReturnValue(
      new Promise<string>((r) => {
        resolveStart = r;
      }),
    );

    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await flush(); // let the availability probe resolve
    await user.click(getByTestId('ai-Summarize'));
    // The request id hasn't resolved yet, so there is nothing to cancel.
    await user.keyboard('{Escape}');
    expect(cancelAiAction).not.toHaveBeenCalled();
    // Once startAiAction resolves, the deferred cancel fires with the new id.
    resolveStart?.('req-late');
    await flush();
    expect(cancelAiAction).toHaveBeenCalledWith('req-late');
  });

  it('does not commit a quick-action result after the inspector has closed', async () => {
    const user = userEvent.setup();
    let resolveRun: ((value: { text: string; warnings: string[] }) => void) | undefined;
    vi.mocked(runQuickAction).mockReturnValue(
      new Promise((r) => {
        resolveRun = r;
      }),
    );

    const { getByTestId, queryByText, rerender } = render(ActionInspector, {
      props: { open: true, target: sample({ id: 'a' }), onClose: () => {} },
    });
    await user.click(getByTestId('quick-FormatJson'));
    // Close before the IPC resolves, then let it resolve into the closed panel.
    await rerender({ open: false, target: sample({ id: 'a' }), onClose: () => {} });
    resolveRun?.({ text: 'stale output', warnings: [] });
    await flush();
    // Reopen on a different target; the superseded result must not appear.
    await rerender({ open: true, target: sample({ id: 'b' }), onClose: () => {} });
    expect(queryByText('stale output')).toBeNull();
  });

  it('clears the work area when re-targeted to another entry while open', async () => {
    // Docked, not modal: clicking another palette row re-targets the live
    // inspector. A result from the previous entry must not linger against the
    // new one, so a target-id change clears the work area back to idle.
    const user = userEvent.setup();
    vi.mocked(runQuickAction).mockResolvedValue({ text: 'first sentence.', warnings: [] });

    const { getByTestId, findByText, queryByText, queryByTestId, rerender } = render(
      ActionInspector,
      { props: { open: true, target: sample({ id: 'a' }), onClose: () => {} } },
    );
    await user.click(getByTestId('quick-SummarizeFirstSentence'));
    expect(await findByText('first sentence.')).toBeTruthy();

    await rerender({ open: true, target: sample({ id: 'b' }), onClose: () => {} });
    expect(queryByText('first sentence.')).toBeNull();
    // The work area is gone entirely (idle), not just emptied.
    expect(queryByTestId('action-run')).toBeNull();
  });

  it('cancels an AI run still starting up when re-targeted, and never adopts it', async () => {
    // The startup window has no request id yet, so close/re-target cannot cancel
    // by id at the time. The `runToken` fence must make the late `startAiAction`
    // resolution cancel the orphaned backend run instead of binding it (and its
    // stream) to the now-current target.
    const user = userEvent.setup();
    let resolveStart: ((id: string) => void) | undefined;
    vi.mocked(startAiAction).mockReturnValue(
      new Promise<string>((r) => {
        resolveStart = r;
      }),
    );

    const { getByTestId, queryByTestId, rerender } = render(ActionInspector, {
      props: { open: true, target: sample({ id: 'a' }), onClose: () => {} },
    });
    await flush(); // let the availability probe resolve
    await user.click(getByTestId('ai-Summarize'));
    // Close (no id to cancel yet) then reopen on a different target.
    await rerender({ open: false, target: sample({ id: 'a' }), onClose: () => {} });
    await rerender({ open: true, target: sample({ id: 'b' }), onClose: () => {} });
    // The orphaned startup finally resolves: cancel it, don't adopt it.
    resolveStart?.('req-orphan');
    await flush();
    expect(cancelAiAction).toHaveBeenCalledWith('req-orphan');
    // No stream was adopted, so the work area stayed idle.
    expect(queryByTestId('action-run')).toBeNull();
  });

  it('includes the AI badge in the accessible name of AI actions', () => {
    const { getByRole } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    // The deterministic "Summarize (first sentence)" and the AI "Summarize"
    // both exist; the AI entry carries the badge in its accessible name so a
    // screen reader still hears that it is the model-backed action.
    expect(getByRole('button', { name: 'Summarize, AI' })).toBeTruthy();
  });

  it('shows the tauriRequired hint when the runtime is unavailable', () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const { getByText } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(getByText('Quick actions require the Tauri runtime.')).toBeTruthy();
  });

  it('auto-focuses the panel when opened', () => {
    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    expect(document.activeElement).toBe(getByTestId('action-inspector'));
  });

  it('invokes onClose when Escape is pressed while idle', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByTestId } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose },
    });
    expect(document.activeElement).toBe(getByTestId('action-inspector'));
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('closes via onClose when the open-actions chord is pressed while focused', async () => {
    // The inspector swallows its own keydowns, so it recognises the same chord
    // that opened it (via `matchesToggle`) and toggles itself shut.
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { getByTestId } = render(ActionInspector, {
      props: {
        open: true,
        target: sample(),
        onClose,
        matchesToggle: (e: KeyboardEvent) => e.key === 'k' && e.metaKey,
      },
    });
    expect(document.activeElement).toBe(getByTestId('action-inspector'));
    await user.keyboard('{Meta>}k{/Meta}');
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

    const { findByText, getByTestId, getByText } = render(ActionInspector, {
      props: { open: true, target: sample(), onClose: () => {} },
    });
    await user.click(getByTestId('quick-FormatJson'));
    await findByText('result body');
    await user.click(getByText('Save as new entry'));
    expect(saveAiResult).toHaveBeenCalledWith('result body');
  });
});

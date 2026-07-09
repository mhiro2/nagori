import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./tauri', () => ({
  invoke: vi.fn(),
}));

import * as commands from './commands';
import { invoke } from './tauri';
import type { AppSettings, SearchRequest } from './types';

const baseSettings = (): AppSettings => ({
  globalHotkey: 'Cmd+Shift+V',
  historyRetentionCount: 1000,
  historyRetentionDays: null,
  maxEntrySizeBytes: 1024 * 1024,
  maxImageEntrySizeBytes: 16 * 1024 * 1024,
  captureKinds: ['text', 'url', 'code', 'image', 'fileList', 'richText', 'unknown'],
  maxTotalBytes: null,
  captureEnabled: true,
  autoPasteEnabled: true,
  pasteFormatDefault: 'preserve',
  pasteDelayMs: 50,
  appDenylist: [],
  regexDenylist: [],
  ai: {
    enabled: false,
    provider: 'disabled',
    allowedActions: [],
    allowStreaming: true,
    requestTimeoutMs: 30000,
    semanticIndexEnabled: false,
    semanticIndexAcPowerOnly: true,
    onboardingDismissed: false,
    allowOpenaiFallbackPrompt: true,
  },
  cliIpcEnabled: true,
  locale: 'en',
  recentOrder: 'by_recency',
  appearance: 'system',
  autoLaunch: false,
  secretHandling: 'store_redacted',
  paletteHotkeys: {},
  secondaryHotkeys: {},
  paletteRowCount: 8,
  showPreviewPane: true,
  showInMenuBar: true,
  clearOnQuit: false,
  permanentDeleteOnDelete: false,
  blockSensitiveCaptures: false,
  otpDetection: true,
  captureInitialClipboardOnLaunch: true,
  autoUpdateCheck: true,
  updateChannel: 'stable',
  maxThumbnailTotalBytes: 64 * 1024 * 1024,
  onboarding: {
    accessibilityPromptedAt: null,
    accessibilityFirstGrantedAt: null,
    completedAt: null,
  },
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(invoke).mockResolvedValue(undefined);
});

describe('command wrappers', () => {
  // Each wrapper is a one-liner over `invoke`. The behaviour worth checking is
  // that the Rust-side command name and the argument serialization stay in
  // sync with the backend; the table below pins the mapping so a future
  // rename in either side trips the test.
  const cases: Array<{ name: string; run: () => Promise<unknown>; cmd: string; args?: unknown }> = [
    {
      name: 'searchClipboard',
      run: () => commands.searchClipboard({ query: 'q', limit: 10 } as SearchRequest),
      cmd: 'search_clipboard',
      args: { request: { query: 'q', limit: 10 } },
    },
    {
      name: 'closePalette',
      run: () => commands.closePalette(),
      cmd: 'close_palette',
      args: undefined,
    },
    {
      name: 'pasteEntryFromPalette',
      run: () => commands.pasteEntryFromPalette('e'),
      cmd: 'paste_entry_from_palette',
      args: { entryId: 'e', format: undefined, forcePaste: undefined },
    },
    {
      name: 'pasteEntryFromPalette (forced)',
      run: () => commands.pasteEntryFromPalette('e', 'plain_text', true),
      cmd: 'paste_entry_from_palette',
      args: { entryId: 'e', format: 'plain_text', forcePaste: true },
    },
    {
      name: 'pasteEntryRepresentationFromPalette',
      run: () => commands.pasteEntryRepresentationFromPalette('e', 'image/png'),
      cmd: 'paste_entry_representation_from_palette',
      args: { entryId: 'e', mime: 'image/png' },
    },
    {
      name: 'listPasteOptions',
      run: () => commands.listPasteOptions('e'),
      cmd: 'list_paste_options',
      args: { entryId: 'e' },
    },
    {
      name: 'copyEntryFromPalette',
      run: () => commands.copyEntryFromPalette('e'),
      cmd: 'copy_entry_from_palette',
      args: { entryId: 'e' },
    },
    {
      name: 'getEntryPreview',
      run: () => commands.getEntryPreview('e'),
      cmd: 'get_entry_preview',
      args: { entryId: 'e', query: undefined },
    },
    {
      name: 'getEntryPreviewFull',
      run: () => commands.getEntryPreviewFull('e'),
      cmd: 'get_entry_preview_full',
      args: { entryId: 'e' },
    },
    {
      name: 'deleteEntry',
      run: () => commands.deleteEntry('id'),
      cmd: 'delete_entry',
      args: { id: 'id' },
    },
    {
      name: 'deleteEntries',
      run: () => commands.deleteEntries(['a', 'b']),
      cmd: 'delete_entries',
      args: { ids: ['a', 'b'] },
    },
    {
      name: 'purgeDeletedEntries',
      run: () => commands.purgeDeletedEntries(),
      cmd: 'purge_deleted_entries',
      args: undefined,
    },
    {
      name: 'copyEntriesCombined',
      run: () => commands.copyEntriesCombined(['a', 'b']),
      cmd: 'copy_entries_combined',
      args: { ids: ['a', 'b'] },
    },
    {
      name: 'pinEntry',
      run: () => commands.pinEntry('id', true),
      cmd: 'pin_entry',
      args: { id: 'id', pinned: true },
    },
    {
      name: 'runQuickAction',
      run: () => commands.runQuickAction('SummarizeFirstSentence', 'e'),
      cmd: 'run_quick_action',
      args: { action: 'SummarizeFirstSentence', entryId: 'e' },
    },
    {
      name: 'startAiAction',
      run: () => commands.startAiAction('Summarize', 'e'),
      cmd: 'start_ai_action',
      args: { action: 'Summarize', entryId: 'e' },
    },
    {
      name: 'cancelAiAction',
      run: () => commands.cancelAiAction('req-1'),
      cmd: 'cancel_ai_action',
      args: { requestId: 'req-1' },
    },
    {
      name: 'getAiAvailability',
      run: () => commands.getAiAvailability(),
      cmd: 'get_ai_availability',
      args: undefined,
    },
    {
      name: 'getSettings',
      run: () => commands.getSettings(),
      cmd: 'get_settings',
      args: undefined,
    },
    {
      name: 'updateSettings',
      run: () => commands.updateSettings(baseSettings()),
      cmd: 'update_settings',
      args: { settings: baseSettings() },
    },
    {
      name: 'hidePalette',
      run: () => commands.hidePalette(),
      cmd: 'hide_palette',
      args: undefined,
    },
    {
      name: 'openSettingsWindow',
      run: () => commands.openSettingsWindow(),
      cmd: 'open_settings',
      args: undefined,
    },
    {
      name: 'getPermissions',
      run: () => commands.getPermissions(),
      cmd: 'get_permissions',
      args: undefined,
    },
    {
      name: 'getCapabilities',
      run: () => commands.getCapabilities(),
      cmd: 'get_capabilities',
      args: undefined,
    },
    {
      name: 'setCaptureEnabled',
      run: () => commands.setCaptureEnabled(false),
      cmd: 'set_capture_enabled',
      args: { enabled: false },
    },
    {
      name: 'saveAiResult',
      run: () => commands.saveAiResult('out'),
      cmd: 'save_ai_result',
      args: { text: 'out' },
    },
    {
      name: 'requestAccessibility',
      run: () => commands.requestAccessibility(true),
      cmd: 'request_accessibility',
      args: { prompt: true },
    },
    {
      name: 'previewEntry',
      run: () => commands.previewEntry('e'),
      cmd: 'preview_entry',
      args: { entryId: 'e' },
    },
  ];

  for (const { name, run, cmd, args } of cases) {
    it(`${name} forwards to invoke('${cmd}')`, async () => {
      await run();
      if (args === undefined) {
        expect(invoke).toHaveBeenCalledWith(cmd);
      } else {
        expect(invoke).toHaveBeenCalledWith(cmd, args);
      }
    });
  }
});

type Deferred = { promise: Promise<void>; resolve: () => void; reject: (e: unknown) => void };
const deferred = (): Deferred => {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

describe('updateSettings module-level FIFO', () => {
  // `save_settings` writes through a multi-connection SQLite pool, so two
  // concurrent `update_settings` IPCs can settle out of order. Tail-chain
  // at the module scope guarantees the second IPC does not even *dispatch*
  // until the first resolves, even when two SettingsView lifecycles overlap
  // (one unmounting, another opening). The unit under test is the chaining
  // itself — verify that we never have more than one in-flight invoke from
  // the wrapper's perspective.

  it('queues a second updateSettings until the first IPC resolves', async () => {
    const first = deferred();
    const second = deferred();
    let callIndex = 0;
    vi.mocked(invoke).mockImplementation(async () => {
      const idx = callIndex;
      callIndex += 1;
      if (idx === 0) return first.promise;
      if (idx === 1) return second.promise;
      throw new Error('unexpected extra invoke');
    });

    const p1 = commands.updateSettings(baseSettings());
    const p2 = commands.updateSettings({ ...baseSettings(), captureEnabled: false });

    // Yield once so `Promise.resolve().then(...)` chains can run; the
    // first invoke should have dispatched but not the second.
    await Promise.resolve();
    await Promise.resolve();
    expect(invoke).toHaveBeenCalledTimes(1);

    // Settle the first; the queue tail unblocks and the second dispatches.
    first.resolve();
    await p1;
    await Promise.resolve();
    await Promise.resolve();
    expect(invoke).toHaveBeenCalledTimes(2);

    second.resolve();
    await p2;
  });

  it('isolates a queued caller from an earlier rejection', async () => {
    // A rejected `updateSettings` (e.g. invalid hotkey) must not poison
    // the tail — subsequent callers should still dispatch. Verify by
    // having the first call fail, then awaiting the second succeeds.
    const first = deferred();
    const second = deferred();
    let callIndex = 0;
    vi.mocked(invoke).mockImplementation(async () => {
      const idx = callIndex;
      callIndex += 1;
      if (idx === 0) return first.promise;
      if (idx === 1) return second.promise;
      throw new Error('unexpected extra invoke');
    });

    const p1 = commands.updateSettings(baseSettings());
    const p2 = commands.updateSettings({ ...baseSettings(), autoLaunch: true });

    first.reject(new Error('invalid hotkey'));
    await expect(p1).rejects.toThrow('invalid hotkey');

    // The second IPC must still dispatch after the rejection drains the tail.
    await Promise.resolve();
    await Promise.resolve();
    expect(invoke).toHaveBeenCalledTimes(2);

    second.resolve();
    await expect(p2).resolves.toBeUndefined();
  });
});

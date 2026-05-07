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
  captureKinds: ['text', 'url', 'code', 'image', 'fileList', 'richText', 'unknown'],
  maxTotalBytes: null,
  captureEnabled: true,
  autoPasteEnabled: true,
  pasteFormatDefault: 'preserve',
  pasteDelayMs: 50,
  appDenylist: [],
  regexDenylist: [],
  localOnlyMode: false,
  aiProvider: 'none',
  aiEnabled: false,
  semanticSearchEnabled: false,
  cliIpcEnabled: true,
  locale: 'en',
  recentOrder: 'by_recency',
  appearance: 'system',
  autoLaunch: false,
  secretHandling: 'store_redacted',
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
      name: 'listRecent',
      run: () => commands.listRecent(20),
      cmd: 'list_recent_entries',
      args: { limit: 20 },
    },
    {
      name: 'listPinned',
      run: () => commands.listPinned(),
      cmd: 'list_pinned_entries',
      args: undefined,
    },
    {
      name: 'getEntry',
      run: () => commands.getEntry('id'),
      cmd: 'get_entry',
      args: { id: 'id' },
    },
    {
      name: 'copyEntry',
      run: () => commands.copyEntry('id'),
      cmd: 'copy_entry',
      args: { id: 'id' },
    },
    {
      name: 'pasteEntry',
      run: () => commands.pasteEntry('id'),
      cmd: 'paste_entry',
      args: { id: 'id', format: undefined },
    },
    {
      name: 'openPalette',
      run: () => commands.openPalette(),
      cmd: 'open_palette',
      args: undefined,
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
      args: { entryId: 'e', format: undefined },
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
      args: { entryId: 'e' },
    },
    {
      name: 'addEntry',
      run: () => commands.addEntry('hello'),
      cmd: 'add_entry',
      args: { text: 'hello' },
    },
    {
      name: 'deleteEntry',
      run: () => commands.deleteEntry('id'),
      cmd: 'delete_entry',
      args: { id: 'id' },
    },
    {
      name: 'pinEntry',
      run: () => commands.pinEntry('id', true),
      cmd: 'pin_entry',
      args: { id: 'id', pinned: true },
    },
    {
      name: 'runAiAction',
      run: () => commands.runAiAction('Summarize', 'e'),
      cmd: 'run_ai_action',
      args: { action: 'Summarize', entryId: 'e' },
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
      name: 'togglePalette',
      run: () => commands.togglePalette(),
      cmd: 'toggle_palette',
      args: undefined,
    },
    {
      name: 'hidePalette',
      run: () => commands.hidePalette(),
      cmd: 'hide_palette',
      args: undefined,
    },
    {
      name: 'getPermissions',
      run: () => commands.getPermissions(),
      cmd: 'get_permissions',
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
      name: 'openAccessibilitySettings',
      run: () => commands.openAccessibilitySettings(),
      cmd: 'open_accessibility_settings',
      args: undefined,
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

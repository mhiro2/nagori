import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
}));

import { getPermissions, getSettings } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { AppSettings, PermissionStatus } from '../lib/types';
import {
  accessibilityGranted,
  accessibilityState,
  aiEnabled,
  captureEnabled,
  refreshSettings,
  settingsState,
} from './settings.svelte';

const baseSettings = (): AppSettings => ({
  globalHotkey: 'Cmd+Shift+V',
  historyRetentionCount: 1000,
  historyRetentionDays: null,
  maxEntrySizeBytes: 1024 * 1024,
  captureKinds: ['text', 'url', 'code', 'image', 'fileList', 'richText', 'unknown'],
  maxTotalBytes: null,
  captureEnabled: false,
  autoPasteEnabled: true,
  pasteFormatDefault: 'preserve',
  pasteDelayMs: 50,
  appDenylist: [],
  regexDenylist: [],
  localOnlyMode: false,
  aiProvider: 'none',
  aiEnabled: true,
  semanticSearchEnabled: false,
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
  captureInitialClipboardOnLaunch: true,
  autoUpdateCheck: true,
  updateChannel: 'stable',
});

const accessibilityPerm = (state: PermissionStatus['state']): PermissionStatus => ({
  kind: 'accessibility',
  state,
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  // Reset the shared state so test order doesn't leak between cases.
  settingsState.settings = undefined;
  settingsState.permissions = [];
  settingsState.loaded = false;
  settingsState.errorMessage = undefined;
});

describe('refreshSettings', () => {
  it('marks loaded but skips IPC when Tauri is unavailable', async () => {
    vi.mocked(isTauri).mockReturnValue(false);
    await refreshSettings();
    expect(getSettings).not.toHaveBeenCalled();
    expect(settingsState.loaded).toBe(true);
    expect(settingsState.settings).toBeUndefined();
  });

  it('hydrates settings + permissions on success', async () => {
    vi.mocked(getSettings).mockResolvedValue(baseSettings());
    vi.mocked(getPermissions).mockResolvedValue([accessibilityPerm('granted')]);
    await refreshSettings();
    expect(settingsState.settings).toMatchObject({ aiEnabled: true });
    expect(settingsState.permissions).toHaveLength(1);
    expect(settingsState.errorMessage).toBeUndefined();
    expect(settingsState.loaded).toBe(true);
  });

  it('surfaces a localized errorMessage when an IPC call rejects', async () => {
    vi.mocked(getSettings).mockRejectedValue(new Error('backend offline'));
    vi.mocked(getPermissions).mockResolvedValue([]);
    await refreshSettings();
    expect(settingsState.errorMessage).toBe('backend offline');
    expect(settingsState.loaded).toBe(true);
  });
});

describe('selectors', () => {
  it('captureEnabled defaults to true when settings are absent', () => {
    expect(captureEnabled()).toBe(true);
  });

  it('captureEnabled mirrors loaded settings', async () => {
    vi.mocked(getSettings).mockResolvedValue(baseSettings());
    vi.mocked(getPermissions).mockResolvedValue([]);
    await refreshSettings();
    expect(captureEnabled()).toBe(false);
  });

  it('aiEnabled defaults to false and reflects loaded settings', async () => {
    expect(aiEnabled()).toBe(false);
    vi.mocked(getSettings).mockResolvedValue(baseSettings());
    vi.mocked(getPermissions).mockResolvedValue([]);
    await refreshSettings();
    expect(aiEnabled()).toBe(true);
  });

  it('accessibilityState/accessibilityGranted reflect the granted state', async () => {
    vi.mocked(getSettings).mockResolvedValue(baseSettings());
    vi.mocked(getPermissions).mockResolvedValue([accessibilityPerm('granted')]);
    await refreshSettings();
    expect(accessibilityState()?.state).toBe('granted');
    expect(accessibilityGranted()).toBe(true);
  });

  it('accessibilityGranted is false when the permission is missing or denied', async () => {
    expect(accessibilityGranted()).toBe(false);
    vi.mocked(getSettings).mockResolvedValue(baseSettings());
    vi.mocked(getPermissions).mockResolvedValue([accessibilityPerm('denied')]);
    await refreshSettings();
    expect(accessibilityGranted()).toBe(false);
  });
});

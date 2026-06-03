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
  applySettingsSnapshot,
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
  ai: {
    enabled: true,
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
  settingsState.partial = false;
  settingsState.settingsErrorMessage = undefined;
  settingsState.permissionsErrorMessage = undefined;
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
    expect(settingsState.settings).toMatchObject({ ai: { enabled: true } });
    expect(settingsState.permissions).toHaveLength(1);
    expect(settingsState.errorMessage).toBeUndefined();
    expect(settingsState.loaded).toBe(true);
  });

  it('keeps the successful leg when only the other leg rejects', async () => {
    vi.mocked(getSettings).mockRejectedValue(new Error('backend offline'));
    vi.mocked(getPermissions).mockResolvedValue([accessibilityPerm('granted')]);
    await refreshSettings();
    // Partial failure must not blanket-clear the side that succeeded —
    // the permissions list is what onboarding keys off, and surfacing
    // defaults here would mis-render the accessibility banner.
    expect(settingsState.permissions).toHaveLength(1);
    expect(settingsState.settings).toBeUndefined();
    expect(settingsState.errorMessage).toBeUndefined();
    expect(settingsState.partial).toBe(true);
    expect(settingsState.settingsErrorMessage).toBe('backend offline');
    expect(settingsState.permissionsErrorMessage).toBeUndefined();
    expect(settingsState.loaded).toBe(true);
  });

  it('surfaces the global errorMessage only when both legs reject', async () => {
    vi.mocked(getSettings).mockRejectedValue(new Error('settings offline'));
    vi.mocked(getPermissions).mockRejectedValue(new Error('permissions offline'));
    await refreshSettings();
    expect(settingsState.errorMessage).toBe('settings offline');
    expect(settingsState.partial).toBe(false);
    expect(settingsState.settingsErrorMessage).toBe('settings offline');
    expect(settingsState.permissionsErrorMessage).toBe('permissions offline');
    expect(settingsState.loaded).toBe(true);
  });

  it('clears stale per-leg errors after a fully successful refresh', async () => {
    vi.mocked(getSettings).mockRejectedValueOnce(new Error('boom'));
    vi.mocked(getPermissions).mockResolvedValueOnce([]);
    await refreshSettings();
    expect(settingsState.partial).toBe(true);
    expect(settingsState.settingsErrorMessage).toBe('boom');

    vi.mocked(getSettings).mockResolvedValueOnce(baseSettings());
    vi.mocked(getPermissions).mockResolvedValueOnce([accessibilityPerm('granted')]);
    await refreshSettings();
    expect(settingsState.partial).toBe(false);
    expect(settingsState.errorMessage).toBeUndefined();
    expect(settingsState.settingsErrorMessage).toBeUndefined();
    expect(settingsState.permissionsErrorMessage).toBeUndefined();
  });
});

describe('applySettingsSnapshot', () => {
  it('adopts the broadcast payload without advancing loaded', () => {
    applySettingsSnapshot({ ...baseSettings(), paletteRowCount: 12 });
    expect(settingsState.settings).toMatchObject({ paletteRowCount: 12 });
    // `loaded` stays owned by `refreshSettings` (which also lands the
    // permission snapshot the accessibility toast keys off); a snapshot
    // flipping it true early would seed that toast from empty permissions.
    expect(settingsState.loaded).toBe(false);
  });

  it('clears a settings-leg error and demotes the global banner to a partial badge', () => {
    settingsState.errorMessage = 'both offline';
    settingsState.settingsErrorMessage = 'settings offline';
    settingsState.permissionsErrorMessage = 'permissions offline';
    applySettingsSnapshot(baseSettings());
    expect(settingsState.settingsErrorMessage).toBeUndefined();
    expect(settingsState.errorMessage).toBeUndefined();
    // Permissions are independent of the snapshot, so a still-failing
    // permission leg keeps the per-leg partial badge up.
    expect(settingsState.partial).toBe(true);
  });

  it('survives a stale in-flight refresh that resolves after it', async () => {
    // A `refreshSettings` (focus / mount) reads the old value and is slow to
    // resolve; a `settings_changed` snapshot lands mid-flight with the new
    // value. When the stale read finally settles it must not revert settings.
    const slot: { resolve?: (value: AppSettings) => void } = {};
    vi.mocked(getSettings).mockImplementationOnce(
      () =>
        new Promise<AppSettings>((resolve) => {
          slot.resolve = resolve;
        }),
    );
    vi.mocked(getPermissions).mockResolvedValue([accessibilityPerm('granted')]);

    const pending = refreshSettings();
    applySettingsSnapshot({ ...baseSettings(), paletteRowCount: 16 });
    expect(settingsState.settings?.paletteRowCount).toBe(16);

    // The stale read resolves with the pre-change value.
    slot.resolve?.({ ...baseSettings(), paletteRowCount: 8 });
    await pending;

    // The fresher snapshot wins; the independent permissions leg still lands.
    expect(settingsState.settings?.paletteRowCount).toBe(16);
    expect(settingsState.permissions).toHaveLength(1);
    expect(settingsState.partial).toBe(false);
    expect(settingsState.errorMessage).toBeUndefined();
  });

  it('keeps a fresh snapshot even if the racing refresh rejected its settings leg', async () => {
    // The settings read fails, but a snapshot already supplied a good value —
    // the failure must not surface an error banner over fresh settings.
    const slot: { reject?: (reason: Error) => void } = {};
    vi.mocked(getSettings).mockImplementationOnce(
      () =>
        new Promise<AppSettings>((_resolve, reject) => {
          slot.reject = reject;
        }),
    );
    vi.mocked(getPermissions).mockResolvedValue([accessibilityPerm('granted')]);

    const pending = refreshSettings();
    applySettingsSnapshot({ ...baseSettings(), paletteRowCount: 20 });

    slot.reject?.(new Error('settings offline'));
    await pending;

    expect(settingsState.settings?.paletteRowCount).toBe(20);
    expect(settingsState.settingsErrorMessage).toBeUndefined();
    expect(settingsState.errorMessage).toBeUndefined();
    expect(settingsState.partial).toBe(false);
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

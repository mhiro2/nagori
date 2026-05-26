import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  requestAccessibility: vi.fn(async () => ({ kind: 'accessibility', state: 'granted' })),
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
}));

import { requestAccessibility } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { AppSettings, PermissionStatus, PlatformCapabilities } from '../lib/types';
import { capabilitiesState } from '../stores/capabilities.svelte';
import { settingsState } from '../stores/settings.svelte';
import OnboardingBanner from './OnboardingBanner.svelte';

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
  aiProvider: 'none',
  aiEnabled: false,
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
  maxThumbnailTotalBytes: 64 * 1024 * 1024,
  onboarding: {
    accessibilityPromptedAt: null,
    accessibilityFirstGrantedAt: null,
    completedAt: null,
  },
});

const accessibility = (state: PermissionStatus['state'], message?: string): PermissionStatus => ({
  kind: 'accessibility',
  state,
  ...(message !== undefined ? { message } : {}),
});

// Minimal capability snapshot whose `platform` flips the banner between the
// macOS and Linux copy. Other fields don't influence the component, so we
// stub them as `unsupported` to satisfy the type.
const capabilities = (platform: PlatformCapabilities['platform']): PlatformCapabilities => {
  const cap = { status: 'unsupported', reason: 'test stub' } as const;
  return {
    platform,
    tier: 'supported',
    captureText: cap,
    captureImage: cap,
    captureFiles: cap,
    writeText: cap,
    writeImage: cap,
    clipboardMultiRepresentationWrite: cap,
    autoPaste: cap,
    globalHotkey: cap,
    frontmostApp: cap,
    permissionsUi: cap,
    updateCheck: cap,
    previewQuickLook: cap,
  };
};

// Pre-populate the store directly so the test stays decoupled from how
// `refreshSettings` short-circuits outside the Tauri runtime.
const seedStore = (perm: PermissionStatus): void => {
  settingsState.settings = baseSettings();
  settingsState.permissions = [perm];
  settingsState.loaded = true;
  settingsState.errorMessage = undefined;
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  settingsState.settings = undefined;
  settingsState.permissions = [];
  settingsState.loaded = false;
  settingsState.errorMessage = undefined;
  capabilitiesState.capabilities = capabilities('macos');
  capabilitiesState.loaded = true;
});

afterEach(cleanup);

describe('OnboardingBanner', () => {
  it('stays hidden before settings have loaded', () => {
    const { container } = render(OnboardingBanner);
    expect(container.querySelector('.onboarding')).toBeNull();
  });

  it('stays hidden when accessibility is granted', async () => {
    seedStore(accessibility('granted'));
    const { container } = render(OnboardingBanner);
    expect(container.querySelector('.onboarding')).toBeNull();
  });

  it('renders when accessibility is denied and offers an open-settings CTA', async () => {
    const user = userEvent.setup();
    seedStore(accessibility('denied'));
    const { getByRole, getByText } = render(OnboardingBanner);
    expect(getByRole('status')).toBeTruthy();
    await user.click(getByText('Open System Settings'));
    expect(requestAccessibility).toHaveBeenCalledTimes(1);
    expect(requestAccessibility).toHaveBeenCalledWith(true);
  });

  it('hides itself when the dismiss button is pressed', async () => {
    const user = userEvent.setup();
    seedStore(accessibility('denied'));
    const { container, getByText } = render(OnboardingBanner);
    expect(container.querySelector('.onboarding')).toBeTruthy();
    await user.click(getByText('Continue without it'));
    expect(container.querySelector('.onboarding')).toBeNull();
  });

  it('skips IPC and silently swallows requestAccessibility errors', async () => {
    const user = userEvent.setup();
    vi.mocked(requestAccessibility).mockRejectedValue(new Error('boom'));
    seedStore(accessibility('denied'));
    const { getByText } = render(OnboardingBanner);
    // Best-effort: the banner shouldn't surface an exception in the UI.
    await user.click(getByText('Open System Settings'));
    expect(requestAccessibility).toHaveBeenCalled();
  });

  it('does not call the command outside the Tauri runtime', async () => {
    const user = userEvent.setup();
    vi.mocked(isTauri).mockReturnValue(false);
    seedStore(accessibility('denied'));
    const { getByText } = render(OnboardingBanner);
    await user.click(getByText('Open System Settings'));
    expect(requestAccessibility).not.toHaveBeenCalled();
  });

  it('on Linux Wayland shows the wtype install hint and hides the settings button', () => {
    capabilitiesState.capabilities = capabilities('linuxWayland');
    seedStore(
      accessibility(
        'denied',
        'wtype was not found on PATH (No such file); auto-paste will fall back to copy-only.',
      ),
    );
    const { queryByText, getByText } = render(OnboardingBanner);
    // Linux-flavoured copy is shown; the macOS settings button is gone.
    expect(getByText('Auto-paste helper required')).toBeTruthy();
    expect(getByText(/wtype was not found on PATH/)).toBeTruthy();
    expect(queryByText('Open System Settings')).toBeNull();
    // Dismiss still works as a sanity check.
    expect(getByText('Continue without it')).toBeTruthy();
  });

  it('falls back to the localised hint when no permission message is supplied', () => {
    capabilitiesState.capabilities = capabilities('linuxWayland');
    seedStore(accessibility('denied'));
    const { getByText } = render(OnboardingBanner);
    expect(getByText(/Install the `wtype` package on a Wayland session/)).toBeTruthy();
  });
});

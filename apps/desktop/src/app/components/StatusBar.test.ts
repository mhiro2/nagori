import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
  getCapabilities: vi.fn(),
  openSettingsWindow: vi.fn(async () => undefined),
}));

import { getPermissions, getSettings, openSettingsWindow } from '../lib/commands';
import type { AppSettings, PermissionStatus, PlatformCapabilities } from '../lib/types';
import { capabilitiesState } from '../stores/capabilities.svelte';
import { refreshSettings, settingsState } from '../stores/settings.svelte';
import StatusBar from './StatusBar.svelte';

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

// Seed the shared stores so the accessibility indicator resolves a known
// 5-state value. `platform` defaults to macOS — the only platform that
// drives the TCC grant the indicator nudges toward.
const seedAccessibility = (
  perm: PermissionStatus | undefined,
  onboardingOverrides: Partial<AppSettings['onboarding']> = {},
  platform: PlatformCapabilities['platform'] = 'macos',
): void => {
  settingsState.settings = baseSettings({
    onboarding: {
      accessibilityPromptedAt: null,
      accessibilityFirstGrantedAt: null,
      completedAt: null,
      ...onboardingOverrides,
    },
  });
  settingsState.permissions = perm ? [perm] : [];
  settingsState.loaded = true;
  capabilitiesState.capabilities = capabilities(platform);
  capabilitiesState.loaded = true;
};

const baseSettings = (overrides: Partial<AppSettings> = {}): AppSettings => ({
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
  ...overrides,
});

beforeEach(() => {
  vi.clearAllMocks();
  settingsState.settings = undefined;
  settingsState.permissions = [];
  settingsState.loaded = false;
  settingsState.errorMessage = undefined;
  capabilitiesState.capabilities = undefined;
  capabilitiesState.loaded = false;
});

afterEach(cleanup);

describe('StatusBar', () => {
  it('renders the entry count and elapsed time on the happy path', () => {
    const { getByText } = render(StatusBar, {
      props: {
        entryCount: 7,
        elapsedMs: 12,
        loading: false,
        errorMessage: undefined,
      },
    });
    expect(getByText(/7/)).toBeTruthy();
    expect(getByText(/12/)).toBeTruthy();
  });

  it('hides the elapsed segment when no timing is available', () => {
    const { container } = render(StatusBar, {
      props: {
        entryCount: 0,
        elapsedMs: undefined,
        loading: false,
        errorMessage: undefined,
      },
    });
    // The middle dot separator only renders when an elapsed segment follows.
    expect(container.querySelector('.left .dot')).toBeNull();
  });

  it('shows the loading hint when a search is in flight', () => {
    const { container } = render(StatusBar, {
      props: { entryCount: 0, elapsedMs: undefined, loading: true, errorMessage: undefined },
    });
    expect(container.querySelector('.left')?.textContent).toMatch(/.+/);
  });

  it('surfaces an errorMessage in place of the count when set', () => {
    const { getByText } = render(StatusBar, {
      props: {
        entryCount: 99,
        elapsedMs: 10,
        loading: false,
        errorMessage: 'backend offline',
      },
    });
    expect(getByText('backend offline')).toBeTruthy();
  });

  it('reflects capture badge state from the settings store', async () => {
    vi.mocked(getSettings).mockResolvedValue(baseSettings({ captureEnabled: false }));
    vi.mocked(getPermissions).mockResolvedValue([]);
    await refreshSettings();

    const { getByText } = render(StatusBar, {
      props: { entryCount: 0, elapsedMs: undefined, loading: false, errorMessage: undefined },
    });
    expect(getByText(/Capture paused/i)).toBeTruthy();
  });
});

describe('StatusBar accessibility indicator', () => {
  const props = { entryCount: 0, elapsedMs: undefined, loading: false, errorMessage: undefined };

  it('stays hidden until the capability snapshot has loaded', () => {
    // Default beforeEach state: no capabilities, no permissions. The
    // indicator must not flash before `get_capabilities` resolves even
    // though the resolver would otherwise read `NotRequested`.
    const { queryByText } = render(StatusBar, { props });
    expect(queryByText(/Accessibility not granted/)).toBeNull();
  });

  it('shows the warning + Setup CTA when Accessibility is not granted', () => {
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    const { getByText, getByRole } = render(StatusBar, { props });
    expect(getByText(/Accessibility not granted/)).toBeTruthy();
    expect(getByRole('button', { name: 'Setup' })).toBeTruthy();
  });

  it('shows the warning for a revoked-after-granted state', () => {
    seedAccessibility(
      { kind: 'accessibility', state: 'denied' },
      {
        accessibilityPromptedAt: '2024-01-01T00:00:00Z',
        accessibilityFirstGrantedAt: '2024-01-02T00:00:00Z',
      },
    );
    const { getByText } = render(StatusBar, { props });
    expect(getByText(/Accessibility not granted/)).toBeTruthy();
  });

  it('hides the warning once Accessibility is granted', () => {
    seedAccessibility({ kind: 'accessibility', state: 'granted' });
    const { queryByText } = render(StatusBar, { props });
    expect(queryByText(/Accessibility not granted/)).toBeNull();
  });

  it('hides the warning on Unavailable platforms', () => {
    seedAccessibility({ kind: 'accessibility', state: 'unsupported' }, {}, 'linuxWayland');
    const { queryByText } = render(StatusBar, { props });
    expect(queryByText(/Accessibility not granted/)).toBeNull();
  });

  it('opens the Settings window on the Setup tab when the CTA is clicked', async () => {
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    const { getByRole } = render(StatusBar, { props });
    await fireEvent.click(getByRole('button', { name: 'Setup' }));
    expect(openSettingsWindow).toHaveBeenCalledWith('setup');
  });
});

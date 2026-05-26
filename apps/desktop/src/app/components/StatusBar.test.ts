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
  setCaptureEnabled: vi.fn(),
}));

import {
  getPermissions,
  getSettings,
  openSettingsWindow,
  setCaptureEnabled,
} from '../lib/commands';
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

  it('toggles capture through the backend and adopts the returned settings', async () => {
    vi.mocked(getSettings).mockResolvedValue(baseSettings({ captureEnabled: true }));
    vi.mocked(getPermissions).mockResolvedValue([]);
    vi.mocked(setCaptureEnabled).mockResolvedValue(baseSettings({ captureEnabled: false }));
    await refreshSettings();

    const { getByRole, findByText } = render(StatusBar, {
      props: { entryCount: 0, elapsedMs: undefined, loading: false, errorMessage: undefined },
    });
    const chip = getByRole('button', { name: /Capture on/i });
    expect(chip.getAttribute('aria-pressed')).toBe('true');

    await fireEvent.click(chip);
    expect(setCaptureEnabled).toHaveBeenCalledWith(false);
    // The chip flips to the paused label once the awaited settings land.
    expect(await findByText(/Capture paused/i)).toBeTruthy();
  });

  it('surfaces a toggle failure on the error channel instead of throwing', async () => {
    vi.mocked(getSettings).mockResolvedValue(baseSettings({ captureEnabled: true }));
    vi.mocked(getPermissions).mockResolvedValue([]);
    vi.mocked(setCaptureEnabled).mockRejectedValue(new Error('backend gone'));
    await refreshSettings();

    const { getByRole } = render(StatusBar, {
      props: { entryCount: 0, elapsedMs: undefined, loading: false, errorMessage: undefined },
    });
    await fireEvent.click(getByRole('button', { name: /Capture on/i }));
    // The rejection lands on settingsState.errorMessage (Palette feeds that
    // into the bar's errorMessage prop) rather than going unhandled.
    await vi.waitFor(() => expect(settingsState.errorMessage).toMatch(/backend gone/));
    // The chip stays on — we don't optimistically flip before the IPC lands.
    expect(getByRole('button', { name: /Capture on/i })).toBeTruthy();
  });
});

describe('StatusBar accessibility indicator', () => {
  const props = { entryCount: 0, elapsedMs: undefined, loading: false, errorMessage: undefined };

  // The warning is now a single clickable chip: its visible label is the
  // short "⚠ Auto-paste off", while the "Accessibility …" detail lives in the
  // accessible name (aria-label) and the `title` tooltip — so the indicator
  // tests key off the button's accessible name.
  it('stays hidden until the capability snapshot has loaded', () => {
    // Default beforeEach state: no capabilities, no permissions. The
    // indicator must not flash before `get_capabilities` resolves even
    // though the resolver would otherwise read `NotRequested`.
    const { queryByRole } = render(StatusBar, { props });
    expect(queryByRole('button', { name: /Accessibility permission required/ })).toBeNull();
  });

  it('shows the warning chip when Accessibility is not granted', () => {
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    const { getByText, getByRole } = render(StatusBar, { props });
    expect(getByText(/Auto-paste off/)).toBeTruthy();
    // Accessible name carries the reason + action the short label omits.
    expect(getByRole('button', { name: /Accessibility permission required/ })).toBeTruthy();
  });

  it('shows the warning for a revoked-after-granted state', () => {
    seedAccessibility(
      { kind: 'accessibility', state: 'denied' },
      {
        accessibilityPromptedAt: '2024-01-01T00:00:00Z',
        accessibilityFirstGrantedAt: '2024-01-02T00:00:00Z',
      },
    );
    const { getByRole } = render(StatusBar, { props });
    expect(getByRole('button', { name: /Accessibility permission required/ })).toBeTruthy();
  });

  it('hides the warning once Accessibility is granted', () => {
    seedAccessibility({ kind: 'accessibility', state: 'granted' });
    const { queryByRole } = render(StatusBar, { props });
    expect(queryByRole('button', { name: /Accessibility permission required/ })).toBeNull();
  });

  it('hides the warning on Unavailable platforms', () => {
    seedAccessibility({ kind: 'accessibility', state: 'unsupported' }, {}, 'linuxWayland');
    const { queryByRole } = render(StatusBar, { props });
    expect(queryByRole('button', { name: /Accessibility permission required/ })).toBeNull();
  });

  it('drops the keyboard hints while the warning chip is showing', () => {
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    const { container } = render(StatusBar, { props });
    expect(container.querySelector('.hints')).toBeNull();
  });

  it('opens the Settings window on the Setup tab when the chip is clicked', async () => {
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    const { getByRole } = render(StatusBar, { props });
    await fireEvent.click(getByRole('button', { name: /Accessibility permission required/ }));
    expect(openSettingsWindow).toHaveBeenCalledWith('setup');
  });
});

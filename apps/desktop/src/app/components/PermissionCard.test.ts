import { cleanup, render, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());

vi.mock('../lib/commands', async () => {
  const { commandsMock } = await import('../test-helpers/moduleMocks');
  return commandsMock({
    requestAccessibility: vi.fn(async () => ({ kind: 'accessibility', state: 'granted' })),
  });
});

vi.mock('../stores/settings.svelte', async () => {
  // Wire a real `$state`-style object so the component sees reactive
  // updates if the test mutates `settingsState`. The shape mirrors the
  // production store closely enough for the component's reads.
  const settingsState = {
    settings: undefined as unknown,
    permissions: [] as unknown[],
    loaded: false,
    errorMessage: undefined,
    partial: false,
    settingsErrorMessage: undefined,
    permissionsErrorMessage: undefined,
  };
  return {
    settingsState,
    accessibilityState: () =>
      (settingsState.permissions as { kind: string }[]).find((p) => p.kind === 'accessibility'),
    accessibilityGranted: () =>
      (settingsState.permissions as { kind: string; state: string }[]).some(
        (p) => p.kind === 'accessibility' && p.state === 'granted',
      ),
    refreshSettings: vi.fn(async () => undefined),
  };
});

import { requestAccessibility } from '../lib/commands';
import { resetPollerForTests } from '../lib/permissions';
import type {
  AppSettings,
  OnboardingSettings,
  PermissionStatus,
  PlatformCapabilities,
} from '../lib/types';
import { capabilitiesState } from '../stores/capabilities.svelte';
import { settingsState } from '../stores/settings.svelte';
import PermissionCard from './PermissionCard.svelte';

const baseSettings = (onboarding: OnboardingSettings): AppSettings => ({
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
  captureInitialClipboardOnLaunch: true,
  autoUpdateCheck: true,
  updateChannel: 'stable',
  maxThumbnailTotalBytes: 64 * 1024 * 1024,
  onboarding,
});

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
    aiActions: cap,
  };
};

const seed = (
  perm: PermissionStatus | undefined,
  onboardingOverrides: Partial<OnboardingSettings> = {},
  platform: PlatformCapabilities['platform'] = 'macos',
): void => {
  settingsState.settings = baseSettings({
    accessibilityPromptedAt: null,
    accessibilityFirstGrantedAt: null,
    completedAt: null,
    ...onboardingOverrides,
  });
  settingsState.permissions = perm ? [perm] : [];
  settingsState.loaded = true;
  capabilitiesState.capabilities = capabilities(platform);
  capabilitiesState.loaded = true;
};

beforeEach(() => {
  vi.clearAllMocks();
  resetPollerForTests();
  settingsState.settings = undefined;
  settingsState.permissions = [];
  settingsState.loaded = false;
  capabilitiesState.capabilities = undefined;
  capabilitiesState.loaded = false;
});

afterEach(() => {
  cleanup();
  resetPollerForTests();
});

describe('PermissionCard accessibility — 5-state rendering', () => {
  it('renders the NotRequested state with the initial Grant CTA', () => {
    seed({ kind: 'accessibility', state: 'notDetermined' });
    const { getByText, getByRole } = render(PermissionCard, { props: { kind: 'accessibility' } });
    expect(getByText('Not requested')).toBeTruthy();
    expect(
      getByText(
        /Nagori has not asked macOS for Accessibility yet\. Press the button below to show the system dialog\./,
      ),
    ).toBeTruthy();
    expect(getByRole('button', { name: /Grant Accessibility…/ })).toBeTruthy();
  });

  it('renders PromptShownNotGranted when the daemon has stamped promptedAt', () => {
    seed(
      { kind: 'accessibility', state: 'notDetermined' },
      { accessibilityPromptedAt: '2024-01-01T00:00:00Z' },
    );
    const { getByText, getByRole } = render(PermissionCard, { props: { kind: 'accessibility' } });
    expect(getByText('Needs action')).toBeTruthy();
    // The grant button morphs into a deep link copy after the first prompt.
    expect(getByRole('button', { name: /Open System Settings/ })).toBeTruthy();
  });

  it('renders Granted and hides the grant / screenshot affordances', () => {
    seed({ kind: 'accessibility', state: 'granted' });
    const { getByText, queryByRole, queryByAltText } = render(PermissionCard, {
      props: { kind: 'accessibility' },
    });
    expect(getByText('Granted')).toBeTruthy();
    expect(getByText('Auto-paste is ready to go.')).toBeTruthy();
    expect(queryByRole('button', { name: /Grant Accessibility|Open System Settings/ })).toBeNull();
    expect(queryByRole('button', { name: /Re-check/ })).toBeNull();
    expect(queryByAltText(/Accessibility/)).toBeNull();
  });

  it('renders RevokedAfterGranted when firstGrantedAt is set but state is denied', () => {
    seed(
      { kind: 'accessibility', state: 'denied' },
      {
        accessibilityPromptedAt: '2024-01-01T00:00:00Z',
        accessibilityFirstGrantedAt: '2024-01-02T00:00:00Z',
      },
    );
    const { getByText } = render(PermissionCard, { props: { kind: 'accessibility' } });
    expect(getByText('Re-enable')).toBeTruthy();
    expect(
      getByText(
        /Nagori was granted Accessibility before\. Re-enable it in System Settings to restore auto-paste\./,
      ),
    ).toBeTruthy();
  });

  it('renders Unavailable with the Linux copy when the platform is linuxWayland', () => {
    seed({ kind: 'accessibility', state: 'unsupported' }, {}, 'linuxWayland');
    const { getByText } = render(PermissionCard, { props: { kind: 'accessibility' } });
    expect(getByText('Not applicable')).toBeTruthy();
    expect(getByText(/Auto-paste on Linux depends on the `wtype` helper/)).toBeTruthy();
  });

  it('renders the Windows copy — never the macOS dialog text — on Windows', () => {
    seed({ kind: 'accessibility', state: 'unsupported' }, {}, 'windows');
    const { getByText, queryByText } = render(PermissionCard, {
      props: { kind: 'accessibility' },
    });
    expect(getByText('Not applicable')).toBeTruthy();
    // The Windows description must replace the macOS "open the dialog" copy.
    expect(getByText(/On Windows, Nagori pastes into the focused app/)).toBeTruthy();
    expect(queryByText(/open the macOS dialog/)).toBeNull();
  });
});

describe('PermissionCard accessibility — interactions', () => {
  it('invokes requestAccessibility(true) when the Grant button is pressed', async () => {
    const user = userEvent.setup();
    seed({ kind: 'accessibility', state: 'notDetermined' });
    const { getByRole } = render(PermissionCard, { props: { kind: 'accessibility' } });
    await user.click(getByRole('button', { name: /Grant Accessibility…/ }));
    expect(requestAccessibility).toHaveBeenCalledTimes(1);
    expect(requestAccessibility).toHaveBeenCalledWith(true);
  });

  it('surfaces an inline error when requestAccessibility rejects', async () => {
    const user = userEvent.setup();
    vi.mocked(requestAccessibility).mockRejectedValueOnce(new Error('TCC unavailable'));
    seed({ kind: 'accessibility', state: 'notDetermined' });
    const { getByRole, findByRole } = render(PermissionCard, {
      props: { kind: 'accessibility' },
    });
    await user.click(getByRole('button', { name: /Grant Accessibility…/ }));
    const alert = await findByRole('alert');
    expect(alert.textContent).toMatch(/Could not start the Accessibility request/);
  });

  it('renders a Re-check button alongside non-Granted states', async () => {
    seed({ kind: 'accessibility', state: 'notDetermined' });
    const { getByRole } = render(PermissionCard, { props: { kind: 'accessibility' } });
    await waitFor(() => {
      expect(getByRole('button', { name: /Re-check/ })).toBeTruthy();
    });
  });

  it('does not show the timeout banner on a Granted card after the 60s poll budget elapses', async () => {
    // A Granted (or Unavailable) card has no further action — the sticky
    // timeout copy would be pure noise. The component must ignore the
    // poller's `timeout` event in those terminal states.
    vi.useFakeTimers();
    try {
      seed({ kind: 'accessibility', state: 'granted' });
      const { queryByRole } = render(PermissionCard, { props: { kind: 'accessibility' } });
      // Drain the initial fetch + the entire 60s poll window so the
      // poller has had ample opportunity to fire its `timeout` event.
      await vi.advanceTimersByTimeAsync(70_000);
      expect(queryByRole('alert')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

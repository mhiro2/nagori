import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', async () => (await import('../test-helpers/moduleMocks')).tauriMock());

vi.mock('../lib/commands', async () => {
  const { commandsMock } = await import('../test-helpers/moduleMocks');
  return commandsMock({ openSettingsWindow: vi.fn(async () => undefined) });
});

import {
  getPermissions,
  getSettings,
  openSettingsWindow,
  setCaptureEnabled,
} from '../lib/commands';
import { buildBindings } from '../lib/keybindings';
import type { AppSettings, PermissionStatus, PlatformCapabilities } from '../lib/types';
import { capabilitiesState } from '../stores/capabilities.svelte';
import {
  captureSkippedState,
  clearCaptureSkip,
  recordCaptureSkip,
} from '../stores/captureSkipped.svelte';
import {
  clearPasteDiagnostics,
  pasteDiagnosticsState,
  recordPasteFailure,
} from '../stores/pasteDiagnostics.svelte';
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
    aiActions: cap,
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
  confirmClearHistory: true,
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
  ...overrides,
});

beforeEach(() => {
  settingsState.settings = undefined;
  settingsState.permissions = [];
  settingsState.loaded = false;
  settingsState.errorMessage = undefined;
  capabilitiesState.capabilities = undefined;
  capabilitiesState.loaded = false;
  clearPasteDiagnostics();
  clearCaptureSkip();
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

  it('renders the pin hint as a button that toggles the selection when wired', async () => {
    const onTogglePin = vi.fn();
    const { getByTestId } = render(StatusBar, {
      props: {
        entryCount: 3,
        elapsedMs: undefined,
        loading: false,
        errorMessage: undefined,
        onTogglePin,
      },
    });
    await fireEvent.click(getByTestId('status-toggle-pin'));
    expect(onTogglePin).toHaveBeenCalledTimes(1);
  });

  it('renders the preview hint as a button that toggles the expanded preview', async () => {
    const onOpenPreview = vi.fn();
    const { getByTestId } = render(StatusBar, {
      props: {
        entryCount: 3,
        elapsedMs: undefined,
        loading: false,
        errorMessage: undefined,
        onOpenPreview,
      },
    });
    await fireEvent.click(getByTestId('status-open-preview'));
    expect(onOpenPreview).toHaveBeenCalledTimes(1);
  });

  it('shows the preview hint as static text (with its accelerator) when not wired', () => {
    capabilitiesState.capabilities = capabilities('macos');
    const { queryByTestId, container } = render(StatusBar, {
      props: {
        entryCount: 3,
        elapsedMs: undefined,
        loading: false,
        errorMessage: undefined,
        bindings: buildBindings({}, 'macos'),
      },
    });
    expect(queryByTestId('status-open-preview')).toBeNull();
    // The label keeps the expanded-preview affordance discoverable even without
    // a click handler wired, and the resolved accelerator shows alongside it.
    const hints = container.querySelector('.hints')?.textContent ?? '';
    expect(hints).toMatch(/Preview/);
    expect(hints).toContain('⌘E');
  });

  it('exposes the preview toggle state via aria-expanded', async () => {
    const { getByTestId, rerender } = render(StatusBar, {
      props: {
        entryCount: 3,
        elapsedMs: undefined,
        loading: false,
        errorMessage: undefined,
        onOpenPreview: vi.fn(),
        previewExpanded: false,
      },
    });
    expect(getByTestId('status-open-preview').getAttribute('aria-expanded')).toBe('false');
    await rerender({
      entryCount: 3,
      elapsedMs: undefined,
      loading: false,
      errorMessage: undefined,
      onOpenPreview: vi.fn(),
      previewExpanded: true,
    });
    expect(getByTestId('status-open-preview').getAttribute('aria-expanded')).toBe('true');
  });

  it('renders the pin hint as static text when no toggle handler is provided', () => {
    const { queryByTestId, container } = render(StatusBar, {
      props: { entryCount: 3, elapsedMs: undefined, loading: false, errorMessage: undefined },
    });
    expect(queryByTestId('status-toggle-pin')).toBeNull();
    // The label still shows in the hints strip so the ⌘P shortcut stays
    // discoverable even without a click handler wired.
    expect(container.querySelector('.hints')?.textContent).toMatch(/Pin/);
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

describe('StatusBar action hints', () => {
  const props = { entryCount: 5, elapsedMs: undefined, loading: false, errorMessage: undefined };

  it('tracks the current confirm behavior across settings, multi-selection, and URL preview', async () => {
    settingsState.settings = baseSettings();
    const { getByTestId, rerender } = render(StatusBar, { props });
    expect(getByTestId('status-confirm-hint').textContent).toContain('Paste');

    settingsState.settings = baseSettings({ autoPasteEnabled: false });
    await vi.waitFor(() =>
      expect(getByTestId('status-confirm-hint').textContent).toContain('Copy'),
    );
    expect(getByTestId('status-confirm-hint').textContent).not.toContain('Paste');

    await rerender({ ...props, selectedCount: 2 });
    expect(getByTestId('status-confirm-hint').textContent).toContain('Copy combined');

    await rerender({ ...props, selectedCount: 2, enterOpensUrl: true });
    expect(getByTestId('status-confirm-hint').textContent).toContain('Open');
    expect(getByTestId('status-confirm-hint').textContent).not.toContain('Copy');
  });

  it.each(['macos', 'windows'] as const)(
    'removes shortcut hints displaced by remaps on %s while keeping mouse actions',
    (platform) => {
      capabilitiesState.capabilities = capabilities(platform);
      const bindings = buildBindings(
        { pin: 'CmdOrCtrl+K', 'open-preview': 'CmdOrCtrl+,', 'copy-without-paste': 'Enter' },
        platform,
      );
      const { getByTestId, queryByTestId } = render(StatusBar, {
        props: {
          ...props,
          bindings,
          onTogglePin: vi.fn(),
          onOpenPreview: vi.fn(),
          onOpenActions: vi.fn(),
          onOpenSettings: vi.fn(),
        },
      });
      expect(getByTestId('status-toggle-pin').querySelector('kbd')?.textContent).toBe(
        platform === 'macos' ? '⌘K' : 'Ctrl+K',
      );
      expect(getByTestId('status-open-preview').querySelector('kbd')?.textContent).toBe(
        platform === 'macos' ? '⌘,' : 'Ctrl+,',
      );
      expect(getByTestId('status-open-actions').querySelector('kbd')).toBeNull();
      expect(getByTestId('status-open-settings').querySelector('kbd')).toBeNull();
      expect(queryByTestId('status-confirm-hint')).toBeNull();
    },
  );
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

  it('keeps the primary hint and Settings available while permission is missing', () => {
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    const onOpenSettings = vi.fn();
    const { getByTestId } = render(StatusBar, { props: { ...props, onOpenSettings } });
    expect(getByTestId('status-confirm-hint')).toBeTruthy();
    expect(getByTestId('status-open-settings')).toBeTruthy();
  });

  it('opens the Settings window on the Setup tab when the chip is clicked', async () => {
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    const { getByRole } = render(StatusBar, { props });
    await fireEvent.click(getByRole('button', { name: /Accessibility permission required/ }));
    expect(openSettingsWindow).toHaveBeenCalledWith('setup');
  });
});

describe('StatusBar paste diagnostic', () => {
  const props = { entryCount: 5, elapsedMs: undefined, loading: false, errorMessage: undefined };

  it('shows the diagnostic chip with a tool-specific hint for a missing tool', () => {
    recordPasteFailure({ reason: 'toolMissing', message: 'raw detail', tool: 'wtype' });
    const { getByTestId } = render(StatusBar, { props });
    const chip = getByTestId('paste-diagnostic-chip');
    expect(chip.getAttribute('data-reason')).toBe('toolMissing');
    // The remediation rides in the title and names the missing tool.
    expect(chip.getAttribute('title')).toMatch(/wtype/);
  });

  it('expands the diagnostic without dismissing it and offers Settings', async () => {
    recordPasteFailure({ reason: 'timeout', message: 'timed out' });
    const { getByTestId, getByText, getByRole } = render(StatusBar, { props });
    await fireEvent.click(getByTestId('paste-diagnostic-chip'));
    expect(pasteDiagnosticsState.failure?.reason).toBe('timeout');
    expect(getByTestId('paste-diagnostic-chip').getAttribute('aria-expanded')).toBe('true');
    expect(getByText(/Auto-paste timed out/)).toBeTruthy();
    await fireEvent.click(getByRole('button', { name: 'Settings' }));
    expect(openSettingsWindow).toHaveBeenCalledWith('setup');
  });

  it('dismisses a diagnostic only through its explicit dismiss control', async () => {
    recordPasteFailure({ reason: 'timeout', message: 'timed out' });
    const { getByRole, queryByTestId } = render(StatusBar, { props });
    await fireEvent.click(getByRole('button', { name: /Dismiss:.*Auto-paste failed/ }));
    expect(pasteDiagnosticsState.failure).toBeNull();
    expect(queryByTestId('paste-diagnostic-chip')).toBeNull();
  });

  it('closes expanded details when a new failure replaces the old one', async () => {
    recordPasteFailure({ reason: 'timeout', message: 'timed out' });
    const { getByTestId } = render(StatusBar, { props });
    await fireEvent.click(getByTestId('paste-diagnostic-chip'));
    recordPasteFailure({ reason: 'previousAppLost', message: 'target gone' });
    await vi.waitFor(() =>
      expect(getByTestId('paste-diagnostic-chip').getAttribute('aria-expanded')).toBe('false'),
    );
  });

  it('keeps the primary hint and Settings available while a paste diagnostic is showing', () => {
    recordPasteFailure({ reason: 'unknown', message: 'failed' });
    const onOpenSettings = vi.fn();
    const { getByTestId } = render(StatusBar, { props: { ...props, onOpenSettings } });
    expect(getByTestId('status-confirm-hint')).toBeTruthy();
    expect(getByTestId('status-open-settings')).toBeTruthy();
  });

  it('folds an accessibilityMissing failure into the accessibility chip (no second chip)', () => {
    // The accessibility chip already explains + fixes this case, so the paste
    // diagnostic must defer rather than stack a redundant second chip.
    seedAccessibility({ kind: 'accessibility', state: 'notDetermined' });
    recordPasteFailure({ reason: 'accessibilityMissing', message: 'grant required' });
    const { queryByTestId, getByRole } = render(StatusBar, { props });
    expect(queryByTestId('paste-diagnostic-chip')).toBeNull();
    expect(getByRole('button', { name: /Accessibility permission required/ })).toBeTruthy();
  });
});

describe('StatusBar capture-skip notice', () => {
  const props = { entryCount: 5, elapsedMs: undefined, loading: false, errorMessage: undefined };

  it('shows the OTP-specific hint when the reasons include a one-time-code match', () => {
    recordCaptureSkip({
      kind: 'secret_redacted_dropped',
      reasons: ['one_time_password_pattern'],
    });
    const { getByTestId } = render(StatusBar, { props });
    const chip = getByTestId('capture-skip-chip');
    expect(chip.getAttribute('data-kind')).toBe('secret_redacted_dropped');
    expect(chip.getAttribute('title')).toMatch(/one-time code/);
  });

  it('shows the generic secret hint for a blocked capture', () => {
    recordCaptureSkip({ kind: 'secret_blocked', reasons: [] });
    const { getByTestId } = render(StatusBar, { props });
    const chip = getByTestId('capture-skip-chip');
    expect(chip.getAttribute('data-kind')).toBe('secret_blocked');
    expect(chip.getAttribute('title')).toMatch(/classified as a secret/);
  });

  it('expands the notice without dismissing it and offers privacy settings', async () => {
    recordCaptureSkip({ kind: 'secret_blocked', reasons: [] });
    const { getByTestId, getByText, getByRole } = render(StatusBar, { props });
    await fireEvent.click(getByTestId('capture-skip-chip'));
    expect(captureSkippedState.notice?.kind).toBe('secret_blocked');
    expect(getByTestId('capture-skip-chip').getAttribute('aria-expanded')).toBe('true');
    expect(getByText(/last copy was classified as a secret/)).toBeTruthy();
    await fireEvent.click(getByRole('button', { name: 'Privacy' }));
    expect(openSettingsWindow).toHaveBeenCalledWith('privacy');
  });

  it('dismisses a capture notice only through its explicit dismiss control', async () => {
    recordCaptureSkip({ kind: 'secret_blocked', reasons: [] });
    const { getByRole, queryByTestId } = render(StatusBar, { props });
    await fireEvent.click(getByRole('button', { name: /Dismiss:.*Copy not saved/ }));
    expect(captureSkippedState.notice).toBeNull();
    expect(queryByTestId('capture-skip-chip')).toBeNull();
  });

  it('defers to a genuine paste-failure chip (privacy notice is lower priority)', () => {
    recordPasteFailure({ reason: 'timeout', message: 'timed out' });
    recordCaptureSkip({ kind: 'secret_blocked', reasons: [] });
    const { getByTestId, queryByTestId } = render(StatusBar, { props });
    expect(getByTestId('paste-diagnostic-chip')).toBeTruthy();
    expect(queryByTestId('capture-skip-chip')).toBeNull();
  });
});

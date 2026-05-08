import { cleanup, render } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  openAccessibilitySettings: vi.fn(async () => undefined),
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
}));

import { openAccessibilitySettings } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { AppSettings, PermissionStatus } from '../lib/types';
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
  paletteHotkeys: {},
  secondaryHotkeys: {},
  paletteRowCount: 8,
  showPreviewPane: true,
  showInMenuBar: true,
  clearOnQuit: false,
  captureInitialClipboardOnLaunch: true,
});

const accessibility = (state: PermissionStatus['state']): PermissionStatus => ({
  kind: 'accessibility',
  state,
});

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
    expect(openAccessibilitySettings).toHaveBeenCalledTimes(1);
  });

  it('hides itself when the dismiss button is pressed', async () => {
    const user = userEvent.setup();
    seedStore(accessibility('denied'));
    const { container, getByText } = render(OnboardingBanner);
    expect(container.querySelector('.onboarding')).toBeTruthy();
    await user.click(getByText('Continue without it'));
    expect(container.querySelector('.onboarding')).toBeNull();
  });

  it('skips IPC and silently swallows openAccessibilitySettings errors', async () => {
    const user = userEvent.setup();
    vi.mocked(openAccessibilitySettings).mockRejectedValue(new Error('boom'));
    seedStore(accessibility('denied'));
    const { getByText } = render(OnboardingBanner);
    // Best-effort: the banner shouldn't surface an exception in the UI.
    await user.click(getByText('Open System Settings'));
    expect(openAccessibilitySettings).toHaveBeenCalled();
  });

  it('does not call the command outside the Tauri runtime', async () => {
    const user = userEvent.setup();
    vi.mocked(isTauri).mockReturnValue(false);
    seedStore(accessibility('denied'));
    const { getByText } = render(OnboardingBanner);
    await user.click(getByText('Open System Settings'));
    expect(openAccessibilitySettings).not.toHaveBeenCalled();
  });
});

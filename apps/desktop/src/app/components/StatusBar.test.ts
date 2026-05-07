import { cleanup, render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
}));

vi.mock('../lib/commands', () => ({
  getSettings: vi.fn(),
  getPermissions: vi.fn(),
}));

import { getPermissions, getSettings } from '../lib/commands';
import type { AppSettings } from '../lib/types';
import { refreshSettings, settingsState } from '../stores/settings.svelte';
import StatusBar from './StatusBar.svelte';

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
  ...overrides,
});

beforeEach(() => {
  vi.clearAllMocks();
  settingsState.settings = undefined;
  settingsState.permissions = [];
  settingsState.loaded = false;
  settingsState.errorMessage = undefined;
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

  it('reflects capture/AI badge state from the settings store', async () => {
    vi.mocked(getSettings).mockResolvedValue(
      baseSettings({ captureEnabled: false, aiEnabled: true }),
    );
    vi.mocked(getPermissions).mockResolvedValue([]);
    await refreshSettings();

    const { getByText } = render(StatusBar, {
      props: { entryCount: 0, elapsedMs: undefined, loading: false, errorMessage: undefined },
    });
    // English copy from the i18n dictionary — "Capture paused" / "AI on".
    expect(getByText(/Capture paused/i)).toBeTruthy();
    expect(getByText(/AI on/i)).toBeTruthy();
  });
});

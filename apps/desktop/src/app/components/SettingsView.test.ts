import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('../lib/tauri', () => ({
  isTauri: vi.fn(() => true),
  subscribe: vi.fn(() => () => {}),
  TAURI_EVENTS: {
    navigate: 'nagori://navigate',
    pasteFailed: 'nagori://paste_failed',
    hotkeyRegisterFailed: 'nagori://hotkey_register_failed',
  },
}));

vi.mock('../lib/commands', () => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
}));

// `onMount` reaches into `@tauri-apps/api/event` to subscribe to hotkey
// failures. The runtime is unavailable in jsdom, so stub the dynamic import.
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import { getSettings, updateSettings } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { AppSettings } from '../lib/types';
import SettingsView from './SettingsView.svelte';

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
  appDenylist: ['1Password'],
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

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  vi.mocked(getSettings).mockResolvedValue(baseSettings());
});

afterEach(cleanup);

describe('SettingsView', () => {
  it('loads settings on mount and hydrates the form fields', async () => {
    const { findByText, findByRole, container } = render(SettingsView);

    expect(await findByText('Save')).toBeTruthy();
    expect(getSettings).toHaveBeenCalled();
    // The hotkey input on the General tab reflects the loaded settings.
    const hotkeyInput = container.querySelector('input[type="text"]') as HTMLInputElement;
    expect(hotkeyInput.value).toBe('Cmd+Shift+V');

    // Switching to Privacy reveals the denylist textarea hydrated from the
    // app-denylist payload.
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);
    await waitFor(() => {
      const textarea = container.querySelector('textarea') as HTMLTextAreaElement;
      expect(textarea?.value).toBe('1Password');
    });
  });

  it('switches the visible fieldset when a tab is clicked', async () => {
    const { findByRole, queryByText } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);
    expect(queryByText('App denylist')).toBeTruthy();
    expect(privacyTab.getAttribute('aria-selected')).toBe('true');
  });

  it('submits changes via updateSettings on save', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByText, container } = render(SettingsView);

    await findByText('Save');
    const captureCheckbox = container.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(captureCheckbox.checked).toBe(true);
    await fireEvent.click(captureCheckbox);

    const form = container.querySelector('form');
    expect(form).toBeTruthy();
    await fireEvent.submit(form as HTMLFormElement);

    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.captureEnabled).toBe(false);
  });

  it('requires confirmation before switching to store_full', async () => {
    const { findByRole, getByDisplayValue } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const select = getByDisplayValue('Store redacted (default)') as HTMLSelectElement;

    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(false);
    await fireEvent.change(select, { target: { value: 'store_full' } });
    expect(confirmSpy).toHaveBeenCalled();
    // Cancelled confirm reverts the dropdown to the previous value.
    expect(select.value).toBe('store_redacted');
    confirmSpy.mockRestore();
  });

  it('commits the store_full switch when the user accepts the warning', async () => {
    const { findByRole, getByDisplayValue, queryByRole } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const select = getByDisplayValue('Store redacted (default)') as HTMLSelectElement;

    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    await fireEvent.change(select, { target: { value: 'store_full' } });
    expect(confirmSpy).toHaveBeenCalled();
    await waitFor(() => {
      expect(queryByRole('alert')).toBeTruthy();
    });
    confirmSpy.mockRestore();
  });

  it('renders the tauriRequired hint outside the runtime', () => {
    vi.mocked(isTauri).mockReturnValue(false);
    const { getByText } = render(SettingsView);
    expect(getByText('Saving settings requires the Tauri runtime.')).toBeTruthy();
  });

  it('surfaces a load error if get_settings rejects', async () => {
    vi.mocked(getSettings).mockRejectedValue(new Error('backend offline'));
    const { findByText } = render(SettingsView);
    expect(await findByText('backend offline')).toBeTruthy();
  });

  it('persists the picked AI provider tag via setProvider', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const aiTab = await findByRole('tab', { name: 'AI' });
    await fireEvent.click(aiTab);

    // The provider <select> exposes the three tags (none/local/remote);
    // switching to "remote" should hydrate the remote provider object on
    // the next save call.
    const select = Array.from(container.querySelectorAll('select')).find((candidate) =>
      Array.from(candidate.options).some((option) => option.value === 'remote'),
    );
    expect(select).toBeTruthy();
    if (select) {
      select.value = 'remote';
      await fireEvent.change(select);
    }

    const form = container.querySelector('form');
    if (form) await fireEvent.submit(form);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.aiProvider).toEqual({ remote: { name: 'openai' } });
  });

  it('renders the CLI tab fieldset and toggles the IPC flag', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const cliTab = await findByRole('tab', { name: 'CLI' });
    await fireEvent.click(cliTab);

    const cliCheckbox = container.querySelector('input[type="checkbox"]');
    expect(cliCheckbox).toBeTruthy();
    if (cliCheckbox) await fireEvent.click(cliCheckbox);

    const form = container.querySelector('form');
    if (form) await fireEvent.submit(form);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.cliIpcEnabled).toBe(false);
  });

  it('writes max-bytes / paste-delay edits from the Advanced tab', async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container } = render(SettingsView);
    const advanced = await findByRole('tab', { name: 'Advanced' });
    await fireEvent.click(advanced);

    const numberInputs = container.querySelectorAll('input[type="number"]');
    expect(numberInputs.length).toBeGreaterThanOrEqual(2);
    const [maxBytes, pasteDelay] = Array.from(numberInputs);
    if (maxBytes) await fireEvent.input(maxBytes, { target: { value: '4096' } });
    if (pasteDelay) await fireEvent.input(pasteDelay, { target: { value: '120' } });

    const form = container.querySelector('form');
    if (form) await fireEvent.submit(form);
    await waitFor(() => {
      expect(updateSettings).toHaveBeenCalled();
    });
    const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
    expect(sent?.maxEntrySizeBytes).toBe(4096);
    expect(sent?.pasteDelayMs).toBe(120);
  });

  it('updates the active locale when the language picker changes', async () => {
    const { findByRole, container } = render(SettingsView);
    await findByRole('tab', { name: 'General' });
    const select = Array.from(container.querySelectorAll('select')).find((candidate) =>
      Array.from(candidate.options).some((option) => option.value === 'ja'),
    );
    expect(select).toBeTruthy();
    if (select) {
      select.value = 'ja';
      await fireEvent.change(select, { target: { value: 'ja' } });
    }
    // After the change the back-to-palette button reflects Japanese copy.
    await waitFor(() => {
      expect(container.textContent).toMatch(/パレット|戻る/);
    });
  });
});

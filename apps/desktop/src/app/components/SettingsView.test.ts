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
  getCapabilities: vi.fn(),
  checkForUpdates: vi.fn(),
}));

// `onMount` reaches into `@tauri-apps/api/event` to subscribe to hotkey
// failures. The runtime is unavailable in jsdom, so stub the dynamic import.
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async () => () => {}),
}));

import { getCapabilities, getSettings, updateSettings } from '../lib/commands';
import { isTauri } from '../lib/tauri';
import type { AppSettings, PlatformCapabilities } from '../lib/types';
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
});

// Capability fixtures mirror what `nagori-platform-{macos,windows,linux}`
// emit from their `capabilities()` adapter. They're not aiming for an
// exhaustive enumeration — just the shape of each status variant so the
// Advanced tab's row + badge renderer is locked down per platform. If
// the backend matrix shifts (e.g. Linux gains in-process global hotkey
// support), bump the fixture and the test will catch any UI drift.
const macosCapabilities = (): PlatformCapabilities => ({
  platform: 'macos',
  tier: 'supported',
  captureText: { status: 'available' },
  captureImage: { status: 'available' },
  captureFiles: { status: 'available' },
  writeText: { status: 'available' },
  writeImage: { status: 'available' },
  clipboardMultiRepresentationWrite: { status: 'available' },
  autoPaste: {
    status: 'requiresPermission',
    permission: 'accessibility',
    message: 'Grant Accessibility access in System Settings.',
  },
  globalHotkey: { status: 'available' },
  frontmostApp: { status: 'available' },
  permissionsUi: { status: 'available' },
  updateCheck: { status: 'available' },
});

const windowsCapabilities = (): PlatformCapabilities => ({
  platform: 'windows',
  tier: 'supported',
  captureText: { status: 'available' },
  captureImage: { status: 'available' },
  captureFiles: { status: 'available' },
  writeText: { status: 'available' },
  writeImage: { status: 'available' },
  // Windows publishes CF_UNICODETEXT + CF_HTML + RTF + CF_DIBV5 + the
  // registered "PNG" companion + CF_HDROP in one transaction (see
  // `crates/nagori-platform-windows/src/capability.rs`), so Preserve
  // copy-back keeps every captured representation alive — multi-rep is
  // Available, not Unsupported.
  clipboardMultiRepresentationWrite: { status: 'available' },
  autoPaste: { status: 'available' },
  globalHotkey: { status: 'available' },
  frontmostApp: { status: 'available' },
  permissionsUi: {
    status: 'unsupported',
    reason:
      'Windows does not gate clipboard / input synthesis behind a user-managed permission UI; the doctor probe is a no-op.',
  },
  updateCheck: { status: 'available' },
});

const linuxWaylandCapabilities = (): PlatformCapabilities => ({
  platform: 'linuxWayland',
  tier: 'supported',
  captureText: { status: 'available' },
  captureImage: { status: 'available' },
  captureFiles: { status: 'available' },
  writeText: { status: 'available' },
  writeImage: { status: 'available' },
  clipboardMultiRepresentationWrite: { status: 'available' },
  autoPaste: {
    status: 'requiresExternalTool',
    tool: 'wtype',
    installHint: 'apt install wtype',
  },
  globalHotkey: {
    status: 'unsupported',
    reason: 'tauri-plugin-global-shortcut is X11-only; pure Wayland sessions fail to register.',
  },
  frontmostApp: {
    status: 'unsupported',
    reason: 'Wayland refuses to expose a portable foreground-surface query.',
  },
  permissionsUi: {
    status: 'unsupported',
    reason:
      'Wayland sessions do not gate clipboard / input synthesis behind a user-managed permission UI; the doctor probe is a no-op.',
  },
  updateCheck: { status: 'available' },
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(isTauri).mockReturnValue(true);
  vi.mocked(getSettings).mockResolvedValue(baseSettings());
  // Default capabilities response so the existing test suite — which
  // already exercises the Advanced tab — has a deterministic stub. The
  // platform-specific tests below override this per-case.
  vi.mocked(getCapabilities).mockResolvedValue(macosCapabilities());
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

  it('blocks save and renders inline guidance when a regex denylist entry is invalid', async () => {
    // The privacy view runs the same preflight `compile_user_regex` does in
    // `nagori-core::policy`, so users see an actionable hint *before* the
    // backend rejects the save with a single opaque `invalid_input` string.
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container, queryByRole } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    // The privacy fieldset has two textareas (app denylist, regex denylist);
    // the regex one is the second.
    const textareas = container.querySelectorAll('textarea');
    expect(textareas.length).toBeGreaterThanOrEqual(2);
    const regexTextarea = textareas[1] as HTMLTextAreaElement;
    // Unbalanced parens are the cheapest way to provoke an invalid-syntax
    // error without bumping the length cap.
    await fireEvent.input(regexTextarea, { target: { value: '(' } });

    await waitFor(() => {
      const alert = queryByRole('alert');
      expect(alert).toBeTruthy();
      expect(alert?.textContent ?? '').toMatch(/Line 1/);
      expect(alert?.textContent ?? '').toMatch(/invalid regex/i);
    });

    const form = container.querySelector('form');
    if (form) await fireEvent.submit(form);

    // Save must short-circuit before reaching the backend so the user
    // doesn't get a generic "Invalid input." round-trip.
    expect(updateSettings).not.toHaveBeenCalled();
  });

  it('reports the original textarea row when blank lines precede the bad regex', async () => {
    // Regression: `linesToList` used to trim + drop empty lines before
    // validation, so the `Line N` label counted non-blank entries instead
    // of the row the user was editing. The validator now keys off the raw
    // split index so a leading blank line still produces `Line 2`.
    vi.mocked(updateSettings).mockResolvedValue();
    const { findByRole, container, queryByRole } = render(SettingsView);
    const privacyTab = await findByRole('tab', { name: 'Privacy' });
    await fireEvent.click(privacyTab);

    const regexTextarea = container.querySelectorAll('textarea')[1] as HTMLTextAreaElement;
    await fireEvent.input(regexTextarea, { target: { value: '\n(' } });

    await waitFor(() => {
      const alert = queryByRole('alert');
      expect(alert).toBeTruthy();
      expect(alert?.textContent ?? '').toMatch(/Line 2/);
      expect(alert?.textContent ?? '').not.toMatch(/Line 1/);
    });
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

  it("round-trips the 'system' locale preference and renders the OS-resolved language", async () => {
    vi.mocked(updateSettings).mockResolvedValue();
    vi.mocked(getSettings).mockResolvedValue({ ...baseSettings(), locale: 'system' });
    // Stub navigator.languages so setLocale('system') resolves to de.
    const originalLanguages = Object.getOwnPropertyDescriptor(window.navigator, 'languages');
    Object.defineProperty(window.navigator, 'languages', {
      value: ['de-DE'],
      configurable: true,
    });

    try {
      const { findByText, container } = render(SettingsView);

      // The Save button renders the German label, proving setLocale('system')
      // routed through navigator.languages to the de dictionary.
      expect(await findByText('Speichern')).toBeTruthy();

      // The dropdown keeps the 'system' preference selected rather than
      // collapsing into the concrete resolved locale.
      const localeSelect = Array.from(container.querySelectorAll('select')).find((candidate) =>
        Array.from(candidate.options).some((option) => option.value === 'system'),
      );
      expect(localeSelect).toBeTruthy();
      expect(localeSelect?.value).toBe('system');

      const form = container.querySelector('form');
      expect(form).toBeTruthy();
      await fireEvent.submit(form as HTMLFormElement);
      await waitFor(() => {
        expect(updateSettings).toHaveBeenCalled();
      });
      const sent = vi.mocked(updateSettings).mock.calls[0]?.[0];
      expect(sent?.locale).toBe('system');
    } finally {
      if (originalLanguages) {
        Object.defineProperty(window.navigator, 'languages', originalLanguages);
      }
    }
  });
});

// Each row label rendered by `capabilityRows` in SettingsView.svelte.
// Order matters for the readout assertion: rows are rendered in this
// order and the test pairs them positionally with the per-platform
// expectations below.
const CAPABILITY_LABELS = [
  'Capture text',
  'Capture image',
  'Capture files',
  'Write text',
  'Write image',
  'Multi-representation copy-back',
  'Auto-paste',
  'Global hotkey',
  'Frontmost app',
  'Permissions UI',
  'Update check',
] as const;

// Status badge labels emitted by `capabilityStatusLabel`. Locks the
// human-readable mapping so a refactor to the enum surface can't
// silently change what shows up in the table.
const STATUS_BADGE = {
  available: 'Available',
  unsupported: 'Unsupported',
  requiresPermission: 'Permission',
  requiresExternalTool: 'External tool',
  experimental: 'Experimental',
} as const;

const readCapabilityTable = (
  container: HTMLElement,
): {
  platform: string;
  tier: string;
  rows: { label: string; status: string; detail: string }[];
} => {
  const meta = container.querySelector('.capability-meta');
  const platform =
    meta?.querySelector('span:nth-of-type(1)')?.textContent?.replace('Platform:', '').trim() ?? '';
  const tier =
    meta?.querySelector('span:nth-of-type(2)')?.textContent?.replace('Tier:', '').trim() ?? '';
  const rows = Array.from(container.querySelectorAll('.capability-table tbody tr')).map((row) => ({
    label: row.querySelector('.capability-label')?.textContent?.trim() ?? '',
    status: row.querySelector('.capability-status')?.textContent?.trim() ?? '',
    detail: row.querySelector('.capability-detail')?.textContent?.trim() ?? '',
  }));
  return { platform, tier, rows };
};

describe('SettingsView Advanced tab — capability table', () => {
  const openAdvancedTab = async (capabilities: PlatformCapabilities) => {
    vi.mocked(getCapabilities).mockResolvedValue(capabilities);
    const view = render(SettingsView);
    const advanced = await view.findByRole('tab', { name: 'Advanced' });
    await fireEvent.click(advanced);
    // Wait for the capability table to mount — `getCapabilities` is
    // resolved off the main render path, so the fieldset only appears
    // once the promise settles.
    await view.findByText('Platform capabilities');
    return view;
  };

  it('renders macOS capabilities — every cap available except Accessibility-gated auto-paste', async () => {
    const { container } = await openAdvancedTab(macosCapabilities());
    const table = readCapabilityTable(container);

    expect(table.platform).toBe('macos');
    expect(table.tier).toBe('supported');
    expect(table.rows.map((r) => r.label)).toEqual([...CAPABILITY_LABELS]);

    // Auto-paste is the only Permission-gated cap on macOS — surfaced
    // so onboarding can prompt the user to grant Accessibility.
    const autoPaste = table.rows.find((r) => r.label === 'Auto-paste');
    expect(autoPaste?.status).toBe(STATUS_BADGE.requiresPermission);
    expect(autoPaste?.detail).toContain('accessibility');
    expect(autoPaste?.detail).toContain('Grant Accessibility access');

    // Every other cap should be `Available`.
    const others = table.rows.filter((r) => r.label !== 'Auto-paste');
    for (const row of others) {
      expect(row.status, `${row.label} should be Available on macOS`).toBe(STATUS_BADGE.available);
    }
  });

  it('renders Windows capabilities — permissions UI Unsupported, updates Available', async () => {
    const { container } = await openAdvancedTab(windowsCapabilities());
    const table = readCapabilityTable(container);

    expect(table.platform).toBe('windows');
    expect(table.tier).toBe('supported');

    const expectedStatus: Record<string, string> = {
      'Capture text': STATUS_BADGE.available,
      'Capture image': STATUS_BADGE.available,
      'Capture files': STATUS_BADGE.available,
      'Write text': STATUS_BADGE.available,
      'Write image': STATUS_BADGE.available,
      'Multi-representation copy-back': STATUS_BADGE.available,
      'Auto-paste': STATUS_BADGE.available,
      'Global hotkey': STATUS_BADGE.available,
      'Frontmost app': STATUS_BADGE.available,
      'Permissions UI': STATUS_BADGE.unsupported,
      'Update check': STATUS_BADGE.available,
    };
    for (const row of table.rows) {
      expect(row.status, `unexpected badge for ${row.label}`).toBe(expectedStatus[row.label]);
    }

    // The unsupported reasons should surface as detail text — the
    // onboarding UI reads these to explain why a feature is greyed out.
    const permissions = table.rows.find((r) => r.label === 'Permissions UI');
    expect(permissions?.detail).toContain('permission UI');
  });

  it('renders Linux Wayland capabilities — wtype external tool + global hotkey unsupported', async () => {
    const { container } = await openAdvancedTab(linuxWaylandCapabilities());
    const table = readCapabilityTable(container);

    expect(table.platform).toBe('linuxWayland');
    expect(table.tier).toBe('supported');

    const expectedStatus: Record<string, string> = {
      'Capture text': STATUS_BADGE.available,
      'Capture image': STATUS_BADGE.available,
      'Capture files': STATUS_BADGE.available,
      'Write text': STATUS_BADGE.available,
      'Write image': STATUS_BADGE.available,
      'Multi-representation copy-back': STATUS_BADGE.available,
      'Auto-paste': STATUS_BADGE.requiresExternalTool,
      'Global hotkey': STATUS_BADGE.unsupported,
      'Frontmost app': STATUS_BADGE.unsupported,
      'Permissions UI': STATUS_BADGE.unsupported,
      'Update check': STATUS_BADGE.available,
    };
    for (const row of table.rows) {
      expect(row.status, `unexpected badge for ${row.label}`).toBe(expectedStatus[row.label]);
    }

    // Auto-paste detail must surface both the tool name and the
    // install hint so the user knows what to apt-install.
    const autoPaste = table.rows.find((r) => r.label === 'Auto-paste');
    expect(autoPaste?.detail).toContain('wtype');
    expect(autoPaste?.detail).toContain('apt install wtype');

    // Global hotkey explanation covers the X11-only upstream constraint
    // that motivates the README's Linux footnote.
    const globalHotkey = table.rows.find((r) => r.label === 'Global hotkey');
    expect(globalHotkey?.detail).toContain('X11-only');
  });
});

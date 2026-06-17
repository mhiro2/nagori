import { vi } from 'vitest';

// Factories for the two modules almost every component/store test has to
// mock. `vi.mock` hoists its factory above imports, so consume these through
// a dynamic import inside the factory:
//
//   vi.mock('../lib/tauri', async () => {
//     const { tauriMock } = await import('../test-helpers/moduleMocks');
//     return tauriMock({ isTauri: vi.fn(() => false) });
//   });
//
// Pass overrides for anything a test needs to deviate from the defaults.

// Mirror of `lib/tauri`'s TAURI_EVENTS map. A literal copy (rather than a
// re-export) because importing the real module here would defeat the mock.
export const TAURI_EVENTS = {
  navigate: 'nagori://navigate',
  clipboardChanged: 'nagori://clipboard_changed',
  pasteFailed: 'nagori://paste_failed',
  hotkeyRegisterFailed: 'nagori://hotkey_register_failed',
  hotkeyRegisterResolved: 'nagori://hotkey_register_resolved',
  settingsChanged: 'nagori://settings_changed',
  aiStarted: 'nagori://ai/started',
  aiDelta: 'nagori://ai/delta',
  aiReplace: 'nagori://ai/replace',
  aiDone: 'nagori://ai/done',
  aiError: 'nagori://ai/error',
  aiCancelled: 'nagori://ai/cancelled',
} as const;

// `lib/tauri` module shape. Defaults: running inside Tauri, subscriptions
// attach instantly (onReady fires synchronously, like the real wrapper after
// the listener resolves) and return a no-op unlisten.
export const tauriMock = (overrides: Record<string, unknown> = {}): Record<string, unknown> => ({
  isTauri: vi.fn(() => true),
  currentWindowLabel: vi.fn((): string | undefined => undefined),
  subscribe: vi.fn((_event: string, _handler: (payload: unknown) => void, onReady?: () => void) => {
    onReady?.();
    return () => {};
  }),
  TAURI_EVENTS,
  ...overrides,
});

// `lib/commands` module shape: every export stubbed as a bare `vi.fn()`.
// Tests give specific commands behaviour via overrides or
// `vi.mocked(...).mockResolvedValue(...)`.
export const commandsMock = (overrides: Record<string, unknown> = {}): Record<string, unknown> => ({
  searchClipboard: vi.fn(),
  closePalette: vi.fn(),
  pasteEntryFromPalette: vi.fn(),
  pasteEntryRepresentationFromPalette: vi.fn(),
  listPasteOptions: vi.fn(),
  copyEntryFromPalette: vi.fn(),
  getEntryPreview: vi.fn(),
  getEntryPreviewFull: vi.fn(),
  addEntry: vi.fn(),
  deleteEntry: vi.fn(),
  deleteEntries: vi.fn(),
  purgeDeletedEntries: vi.fn(),
  copyEntriesCombined: vi.fn(),
  pinEntry: vi.fn(),
  runQuickAction: vi.fn(),
  startAiAction: vi.fn(),
  cancelAiAction: vi.fn(),
  getAiAvailability: vi.fn(),
  getSemanticIndexStatus: vi.fn(),
  rebuildSemanticIndex: vi.fn(),
  getSettings: vi.fn(),
  passwordManagerPreset: vi.fn(),
  updateSettings: vi.fn(),
  hidePalette: vi.fn(),
  openSettingsWindow: vi.fn(),
  getPermissions: vi.fn(),
  getCapabilities: vi.fn(),
  setCaptureEnabled: vi.fn(),
  saveAiResult: vi.fn(),
  requestAccessibility: vi.fn(),
  openUrlExternal: vi.fn(),
  previewEntry: vi.fn(),
  checkForUpdates: vi.fn(),
  cliInstallStatus: vi.fn(),
  installCli: vi.fn(),
  lastHotkeyFailure: vi.fn(),
  ...overrides,
});

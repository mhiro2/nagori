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

// The real `lib/tauri` event-name map. Taken from the actual module (not a
// hand-kept copy) so a component subscribing to a key the production map
// lacks — or a renamed wire value — fails against the mock too, instead of
// two missing keys comparing equal as `undefined`.
export const { TAURI_EVENTS } =
  await vi.importActual<typeof import('../lib/tauri')>('../lib/tauri');

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
  cancelSearch: vi.fn(async () => undefined),
  listSourceApps: vi.fn(async () => []),
  closePalette: vi.fn(),
  fitPaletteHeight: vi.fn(async () => undefined),
  notifyPasteFailure: vi.fn(async () => undefined),
  pasteEntryFromPalette: vi.fn(),
  pasteEntryRepresentationFromPalette: vi.fn(),
  listPasteOptions: vi.fn(),
  copyEntryFromPalette: vi.fn(),
  copyTextFromPalette: vi.fn(async () => undefined),
  pasteTextFromPalette: vi.fn(async () => undefined),
  getEntryPreview: vi.fn(),
  getEntryPreviewFull: vi.fn(),
  addEntry: vi.fn(),
  deleteEntry: vi.fn(),
  deleteEntries: vi.fn(),
  purgeDeletedEntries: vi.fn(),
  clearHistory: vi.fn(),
  copyEntriesCombined: vi.fn(),
  pinEntry: vi.fn(),
  getEntryLabel: vi.fn(async () => null),
  setEntryLabel: vi.fn(async () => undefined),
  getEditableEntryText: vi.fn(),
  updateEntryText: vi.fn(async () => undefined),
  recognizeImageText: vi.fn(),
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
  setConfirmClearHistory: vi.fn(),
  saveAiResult: vi.fn(),
  requestAccessibility: vi.fn(),
  openUrlExternal: vi.fn(),
  previewEntry: vi.fn(),
  checkForUpdates: vi.fn(),
  openReleasePage: vi.fn(async () => undefined),
  installUpdate: vi.fn(async () => undefined),
  cliInstallStatus: vi.fn(),
  installCli: vi.fn(),
  lastHotkeyFailure: vi.fn(),
  paletteShortcutBound: vi.fn(async () => true),
  ...overrides,
});

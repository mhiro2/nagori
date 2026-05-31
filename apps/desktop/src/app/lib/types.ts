// Frontend data contracts mirrored from `nagori-core` domain types.

export type ContentKind = 'text' | 'url' | 'code' | 'image' | 'fileList' | 'richText' | 'unknown';

export const CONTENT_KINDS: readonly ContentKind[] = [
  'text',
  'url',
  'code',
  'image',
  'fileList',
  'richText',
  'unknown',
];

export type Sensitivity = 'Unknown' | 'Public' | 'Private' | 'Secret' | 'Blocked';

export type SearchMode = 'Auto' | 'Recent' | 'Exact' | 'Fuzzy' | 'FullText' | 'Semantic';

export type RankReason =
  | 'ExactMatch'
  | 'PrefixMatch'
  | 'SubstringMatch'
  | 'FullTextMatch'
  | 'NgramMatch'
  | 'SemanticMatch'
  | 'Recent'
  | 'FrequentlyUsed'
  | 'Pinned';

// Deterministic on-device quick actions (no language model). Mirrors
// `nagori_core::QuickActionId`.
export type QuickActionId =
  | 'FormatJson'
  | 'ExtractTasks'
  | 'RedactSecrets'
  | 'SummarizeFirstSentence';

// Model-backed AI actions resolved through the engine. Mirrors
// `nagori_core::AiActionId`. The text-generation actions (everything but
// `Translate`) and `Translate` are wired on macOS; on other platforms they
// report `capabilityMismatch`.
export type AiActionId =
  | 'Summarize'
  | 'Translate'
  | 'Rewrite'
  | 'FormatMarkdown'
  | 'ExtractTasks'
  | 'ExplainCode';

// Which provider family backs the AI actions. Mirrors
// `nagori_core::AiProviderKind`.
export type AiProviderKind = 'disabled' | 'appleNative' | 'openAiCompatible';

// Mirrors `nagori_core::AiSettings`.
export type AiSettings = {
  enabled: boolean;
  provider: AiProviderKind;
  allowedActions: AiActionId[];
  allowStreaming: boolean;
  requestTimeoutMs: number;
  semanticIndexEnabled: boolean;
  semanticIndexAcPowerOnly: boolean;
  onboardingDismissed: boolean;
  allowOpenaiFallbackPrompt: boolean;
};

// Coarse state of the semantic index. Mirrors `nagori_core::SemanticIndexState`
// (snake_case via serde).
export type SemanticIndexState =
  | 'disabled'
  | 'unsupported'
  | 'unavailable'
  | 'ready'
  | 'indexing'
  | 'paused';

// Mirrors the desktop `SemanticIndexStatusDto`.
export type SemanticIndexStatus = {
  state: SemanticIndexState;
  indexed: number;
  pending: number;
  total: number;
  model?: string;
};

// Per-action availability status. Mirrors `nagori_core::PerActionStatus`
// (snake_case via serde).
export type AiActionStatus =
  | 'available'
  | 'disabled_by_settings'
  | 'capability_mismatch'
  | 'os_unavailable'
  | 'asset_missing'
  | 'language_unsupported'
  | 'not_configured'
  | 'unknown';

export type AiActionAvailability = {
  action: AiActionId;
  status: AiActionStatus;
  available: boolean;
  remediation?: string;
};

export type AiOverallStatus = 'available' | 'unavailable' | 'disabled';

export type AiAvailability = {
  provider: AiProviderKind;
  overallStatus: AiOverallStatus;
  actions: AiActionAvailability[];
};

// Payloads for the `nagori://ai/*` streaming events emitted while a model-backed
// action runs.
export type AiStartedEvent = { requestId: string };
export type AiDeltaEvent = { requestId: string; seq: number; text: string };
export type AiReplaceEvent = { requestId: string; seq: number; text: string };
export type AiDoneEvent = {
  requestId: string;
  finalText: string;
  createdEntryId?: string | null;
  warnings: string[];
};
export type AiErrorEvent = {
  requestId: string;
  code: string;
  message: string;
  remediation?: string | null;
};
export type AiCancelledEvent = { requestId: string };

export type SearchResultDto = {
  id: string;
  kind: ContentKind;
  preview: string;
  score: number;
  createdAt: string;
  pinned: boolean;
  sensitivity: Sensitivity;
  rankReasons: RankReason[];
  sourceAppName?: string;
  representationSummary: RepresentationSummary[];
};

export type RepresentationRole = 'primary' | 'plainFallback' | 'alternative';

export type RepresentationSummary = {
  mimeType: string;
  role: RepresentationRole;
  byteCount: number;
};

export type EntryDto = {
  id: string;
  kind: ContentKind;
  text?: string;
  preview: string;
  createdAt: string;
  updatedAt: string;
  lastUsedAt?: string;
  useCount: number;
  pinned: boolean;
  sourceAppName?: string;
  sensitivity: Sensitivity;
  representationSummary: RepresentationSummary[];
};

export type EntryPreviewBody =
  | { type: 'text'; text: string }
  | { type: 'code'; text: string; language?: string | null }
  | {
      type: 'url';
      url: string;
      domain?: string | null;
      // Three-way decomposition supplied by `UrlParts::from_raw` on the
      // backend. All four extra fields are absent when the URL failed
      // to parse, in which case the renderer falls back to the flat
      // `url` string.
      scheme?: string | null;
      hostDisplay?: string | null;
      // Only present when the IDN punycode form differs from
      // `hostDisplay` — the renderer surfaces a phishing-resistance
      // badge when this is truthy.
      hostPunycode?: string | null;
      pathAndQuery?: string | null;
    }
  | {
      type: 'image';
      mimeType?: string | null;
      byteCount: number;
      width?: number | null;
      height?: number | null;
    }
  | { type: 'fileList'; paths: string[]; total: number }
  | { type: 'richText'; text: string }
  | { type: 'unknown'; text: string };

// Mirrors `TruncationDto` in `apps/desktop/src-tauri/src/dto.rs`. The
// frontend dispatches on `kind` and falls back to `truncated: boolean`
// only when older builds emit the DTO without this field.
export type PreviewTruncation =
  | { kind: 'none' }
  | { kind: 'headOnly' }
  | { kind: 'headAndTail'; elidedBytes: number };

export type EntryPreviewDto = {
  id: string;
  kind: ContentKind;
  title?: string | null;
  previewText: string;
  body: EntryPreviewBody;
  metadata: {
    byteCount: number;
    charCount: number;
    lineCount: number;
    truncated: boolean;
    truncation?: PreviewTruncation;
    sensitive: boolean;
    fullContentAvailable: boolean;
    domain?: string | null;
    language?: string | null;
    // best-effort signal: when the body was truncated, indicates whether
    // the current search query matches text inside the elided middle.
    // `undefined` when no query was passed or nothing was elided.
    elidedContainsMatch?: boolean;
  };
};

export type SearchFilters = {
  kinds?: ContentKind[];
  pinnedOnly?: boolean;
  sourceApp?: string;
  createdAfter?: string;
  createdBefore?: string;
};

export type SearchRequest = {
  query: string;
  mode?: SearchMode;
  limit?: number;
  filters?: SearchFilters;
};

export type SearchResponse = {
  results: SearchResultDto[];
  totalCandidates: number;
  // Timing breakdown from `search_clipboard`. `totalElapsedMs` is the
  // end-to-end value the status bar shows; `searchElapsedMs` /
  // `summaryElapsedMs` split it into the search pipeline vs. representation
  // summary hydration for diagnosing which dominates a slow query.
  searchElapsedMs: number;
  summaryElapsedMs: number;
  totalElapsedMs: number;
};

// Mirrors `SourceAppIdKind` (snake_case via serde) in `nagori-core`.
// Identifies which platform-specific identifier an `AppDenyRule` carries
// so the daemon can match exact bundle IDs / exe basenames instead of
// substring-matching a free-form display name.
export type SourceAppIdKind =
  | 'macos_bundle_id'
  | 'windows_exe_name'
  | 'windows_executable_path'
  | 'linux_desktop_id'
  | 'linux_flatpak_id'
  | 'x11_wm_class';

// `manual` (default) flags rules typed in by the user; `preset` flags
// rules pulled from a bundled list (e.g. password managers). The
// distinction lets the UI tell preset-managed entries apart from
// custom patterns when re-rendering the form.
export type RuleSource = 'manual' | 'preset';

// Internally-tagged union: `type: 'source_app'` carries a typed identifier
// (bundle ID, exe name) for exact-match blocking, `type: 'pattern'` keeps
// the legacy substring behaviour so old user-typed entries still work.
export type AppDenyRule =
  | {
      type: 'source_app';
      kind: SourceAppIdKind;
      value: string;
      label?: string | null;
      source?: RuleSource;
    }
  | { type: 'pattern'; value: string };

// Concrete UI locales that have a translation dictionary on disk.
export type Locale = 'en' | 'ja' | 'ko' | 'zh-Hans' | 'zh-Hant' | 'de' | 'fr' | 'es';

// What the persisted setting can hold. `'system'` is a sentinel — the
// frontend resolves it to one of the concrete locales above by negotiating
// the OS / WebView language preferences on each load, so toggling the OS
// language follows through without touching settings.
export type LocaleSetting = Locale | 'system';

// Wire format mirrors `SecretHandlingDto` (snake_case via serde) — the
// camelCase rename on the parent struct only affects field names, not enum
// variants. Keep this string-union in lockstep with the Rust enum.
export type SecretHandling = 'block' | 'store_redacted' | 'store_full';
export type PasteFormat = 'preserve' | 'plain_text';
export type RecentOrder = 'by_recency' | 'by_use_count' | 'pinned_first_then_recency';
export type Appearance = 'light' | 'dark' | 'system';
export type UpdateChannel = 'stable';

export type PaletteHotkeyAction =
  | 'pin'
  | 'delete'
  | 'paste-as-plain'
  | 'copy-without-paste'
  | 'clear'
  | 'open-preview';

export type SecondaryHotkeyAction = 'repaste-last' | 'clear-history';

// Mirrors `OnboardingSettings` in `nagori-core`. The three timestamps are
// sticky onboarding markers stamped by the daemon (never by the frontend);
// the desktop must still round-trip them through `updateSettings` so a
// concurrent settings write does not zero them via `#[serde(default)]` on
// the Rust DTO. All RFC3339 strings; `null` means "never observed".
export type OnboardingSettings = {
  accessibilityPromptedAt: string | null;
  accessibilityFirstGrantedAt: string | null;
  completedAt: string | null;
};

export type AppSettings = {
  globalHotkey: string;
  historyRetentionCount: number;
  historyRetentionDays: number | null;
  maxEntrySizeBytes: number;
  captureKinds: ContentKind[];
  maxTotalBytes: number | null;
  captureEnabled: boolean;
  autoPasteEnabled: boolean;
  pasteFormatDefault: PasteFormat;
  pasteDelayMs: number;
  appDenylist: AppDenyRule[];
  regexDenylist: string[];
  ai: AiSettings;
  cliIpcEnabled: boolean;
  locale: LocaleSetting;
  recentOrder: RecentOrder;
  appearance: Appearance;
  autoLaunch: boolean;
  secretHandling: SecretHandling;
  paletteHotkeys: Partial<Record<PaletteHotkeyAction, string>>;
  secondaryHotkeys: Partial<Record<SecondaryHotkeyAction, string>>;
  paletteRowCount: number;
  showPreviewPane: boolean;
  showInMenuBar: boolean;
  clearOnQuit: boolean;
  captureInitialClipboardOnLaunch: boolean;
  autoUpdateCheck: boolean;
  updateChannel: UpdateChannel;
  maxThumbnailTotalBytes: number | null;
  onboarding: OnboardingSettings;
};

export type UpdateInfo = {
  version: string;
  currentVersion: string;
  releaseNotes?: string | null;
  downloadSupported: boolean;
};

// Mirrors `CliInstallStatusDto` — the read-only state behind the Settings →
// CLI install affordance. `supported` is false on Windows (no one-click
// installer yet); `bundled` is false under `tauri dev`, where the sidecar is
// not staged beside the dev binary.
export type CliInstallStatus = {
  supported: boolean;
  bundled: boolean;
  installed: boolean;
  installedPath: string;
  binDir: string;
  onPath: boolean;
};

// Mirrors `CliInstallResultDto` — returned by a successful `install_cli`.
export type CliInstallResult = {
  installedPath: string;
  binDir: string;
  sourcePath: string;
  onPath: boolean;
};

export type PermissionKind =
  | 'accessibility'
  | 'inputMonitoring'
  | 'clipboard'
  | 'notifications'
  | 'autoLaunch';

export type PermissionState = 'granted' | 'denied' | 'notDetermined' | 'unsupported';

export type PermissionStatus = {
  kind: PermissionKind;
  state: PermissionState;
  message?: string;
  // Machine-readable diagnostic / deep-link hints. `reasonCode`
  // is a stable identifier the UI can branch on (e.g.
  // `"accessibility_not_prompted"`), `setupRoute` is the in-app route
  // that walks the user through the fix, `docsUrl` points to the public
  // docs for the same kind. All optional — older platform adapters
  // continue to omit them.
  reasonCode?: string;
  setupRoute?: string;
  docsUrl?: string;
};

// Mirrors the `nagori://hotkey_register_failed` emit envelope and the
// `last_hotkey_failure` query response. `kind` is absent for the primary
// palette shortcut and `"secondary"` for repaste-last / clear-history
// accelerators — the UI collapses both into a single error surface but
// the tag is preserved for future routing. `action` carries the
// kebab-case wire value of the failing secondary action (absent for
// primaries); the store reads it so a later resolved event scoped to a
// different secondary action doesn't wipe the displayed banner.
export type HotkeyFailure = {
  hotkey: string;
  error: string;
  kind?: string;
  action?: string;
};

export type Platform = 'macos' | 'windows' | 'linuxWayland' | 'unsupported';

export type SupportTier = 'supported' | 'experimental' | 'unsupported';

export type Capability =
  | { status: 'available' }
  | { status: 'unsupported'; reason: string }
  | { status: 'requiresPermission'; permission: PermissionKind; message: string }
  | { status: 'requiresExternalTool'; tool: string; installHint?: string }
  | { status: 'experimental'; message: string };

export type PlatformCapabilities = {
  platform: Platform;
  tier: SupportTier;
  captureText: Capability;
  captureImage: Capability;
  captureFiles: Capability;
  writeText: Capability;
  writeImage: Capability;
  clipboardMultiRepresentationWrite: Capability;
  autoPaste: Capability;
  globalHotkey: Capability;
  frontmostApp: Capability;
  permissionsUi: Capability;
  updateCheck: Capability;
  previewQuickLook: Capability;
};

export type CommandError = {
  code: string;
  message: string;
  recoverable: boolean;
};

export type AiActionResult = {
  text: string;
  createdEntryId?: string;
  warnings: string[];
};

// Frontend data contracts mirrored from `nagori-core` domain types.

export type ContentKind = 'text' | 'url' | 'code' | 'image' | 'fileList' | 'richText' | 'unknown';

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

export type AiActionId =
  | 'Summarize'
  | 'Translate'
  | 'FormatJson'
  | 'FormatMarkdown'
  | 'ExplainCode'
  | 'Rewrite'
  | 'ExtractTasks'
  | 'RedactSecrets';

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
};

export type EntryPreviewBody =
  | { type: 'text'; text: string }
  | { type: 'code'; text: string; language?: string | null }
  | { type: 'url'; url: string; domain?: string | null }
  | {
      type: 'image';
      mimeType?: string | null;
      byteCount: number;
      width?: number | null;
      height?: number | null;
    }
  | { type: 'fileList'; paths: string[] }
  | { type: 'richText'; text: string }
  | { type: 'unknown'; text: string };

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
    sensitive: boolean;
    fullContentAvailable: boolean;
    domain?: string | null;
    language?: string | null;
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
  elapsedMs: number;
};

export type AiProviderSetting = 'none' | 'local' | { remote: { name: string } };

export type LocaleSetting = 'en' | 'ja' | 'ko' | 'zh-Hans';

// Wire format mirrors `SecretHandlingDto` (snake_case via serde) — the
// camelCase rename on the parent struct only affects field names, not enum
// variants. Keep this string-union in lockstep with the Rust enum.
export type SecretHandling = 'block' | 'store_redacted' | 'store_full';
export type PasteFormat = 'preserve' | 'plain_text';
export type RecentOrder = 'by_recency' | 'by_use_count' | 'pinned_first_then_recency';
export type Appearance = 'light' | 'dark' | 'system';

export type PaletteHotkeyAction =
  | 'pin'
  | 'delete'
  | 'paste-as-plain'
  | 'copy-without-paste'
  | 'clear'
  | 'open-preview';

export type SecondaryHotkeyAction = 'repaste-last' | 'clear-history';

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
  appDenylist: string[];
  regexDenylist: string[];
  localOnlyMode: boolean;
  aiProvider: AiProviderSetting;
  aiEnabled: boolean;
  semanticSearchEnabled: boolean;
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

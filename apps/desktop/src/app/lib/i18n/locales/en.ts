// English is the base locale. The `Messages` interface defines the canonical
// translation key set; every other dictionary must satisfy it structurally.

export type CountFormatter = (count: number) => string;

export type Messages = {
  palette: {
    placeholder: string;
    searching: string;
    resultCount: CountFormatter;
    elapsed: (ms: number) => string;
    empty: string;
    fallback: string;
    hints: {
      navigate: string;
      paste: string;
      actions: string;
      settings: string;
    };
  };
  preview: {
    empty: string;
    loading: string;
    truncated: string;
    fields: {
      id: string;
      sensitivity: string;
      source: string;
      size: string;
      rank: string;
    };
    none: string;
    image: {
      loading: string;
      unavailable: string;
    };
  };
  status: {
    captureOn: string;
    capturePaused: string;
    aiOn: string;
    aiOff: string;
    entryCount: CountFormatter;
  };
  actionMenu: {
    title: string;
    actions: {
      Summarize: string;
      Translate: string;
      FormatJson: string;
      FormatMarkdown: string;
      ExplainCode: string;
      Rewrite: string;
      ExtractTasks: string;
      RedactSecrets: string;
    };
    tauriRequired: string;
    resultTitle: string;
    copyResult: string;
    copied: string;
    saveResult: string;
    saved: string;
    closeResult: string;
    runFailed: string;
  };
  onboarding: {
    title: string;
    description: string;
    accessibilityRequired: string;
    accessibilityHint: string;
    autoPasteDisabled: string;
    notificationsHint: string;
    openSettings: string;
    dismiss: string;
  };
  settings: {
    title: string;
    backToPalette: string;
    loading: string;
    saving: string;
    save: string;
    tauriRequired: string;
    tabs: {
      general: string;
      privacy: string;
      ai: string;
      cli: string;
      advanced: string;
    };
    capture: {
      legend: string;
      enabled: string;
      autoPaste: string;
      pasteFormatDefault: string;
      pasteFormatOptions: { preserve: string; plain_text: string };
      hotkey: string;
    };
    retention: {
      legend: string;
      maxCount: string;
      maxDays: string;
      maxDaysPlaceholder: string;
      maxDaysHelp: string;
      maxTotalBytes: string;
      maxTotalBytesPlaceholder: string;
      maxTotalBytesHelp: string;
      maxBytes: string;
      pasteDelayMs: string;
    };
    privacy: {
      legend: string;
      localOnly: string;
      appDenylist: string;
      appDenylistHelp: string;
      regexDenylist: string;
      regexDenylistHelp: string;
      secretHandling: string;
      secretHandlingHelp: string;
      secretHandlingOptions: {
        block: string;
        store_redacted: string;
        store_full: string;
      };
      captureKinds: string;
      captureKindsHelp: string;
      captureKindOptions: {
        text: string;
        url: string;
        code: string;
        image: string;
        fileList: string;
        richText: string;
        unknown: string;
      };
      storeFullWarning: string;
      storeFullConfirm: string;
    };
    ai: {
      legend: string;
      enabled: string;
      provider: string;
      providers: { none: string; local: string; remote: string };
      semanticSearch: string;
    };
    cli: { legend: string; ipcEnabled: string };
    appearance: {
      legend: string;
      locale: string;
      theme: string;
      themeOptions: { system: string; light: string; dark: string };
      recentOrder: string;
      recentOrderOptions: {
        by_recency: string;
        by_use_count: string;
        pinned_first_then_recency: string;
      };
    };
    integration: {
      legend: string;
      autoLaunch: string;
      autoLaunchHelp: string;
    };
  };
  keybindings: {
    selectNext: string;
    selectPrev: string;
    selectFirst: string;
    selectLast: string;
    confirm: string;
    openActions: string;
    togglePin: string;
    delete: string;
    openSettings: string;
    close: string;
  };
  time: {
    justNow: string;
    minutesAgo: CountFormatter;
    hoursAgo: CountFormatter;
    daysAgo: CountFormatter;
  };
  errors: {
    unknown: string;
    storage: string;
    search: string;
    platform: string;
    permission: string;
    ai: string;
    policy: string;
    notFound: string;
    invalidInput: string;
    unsupported: string;
  };
  locales: {
    en: string;
    ja: string;
    ko: string;
    'zh-Hans': string;
  };
};

export const en: Messages = {
  palette: {
    placeholder: 'Search history…',
    searching: 'Searching…',
    resultCount: (count) => (count === 1 ? '1 result' : `${count.toLocaleString('en')} results`),
    elapsed: (ms) => `${ms.toFixed(0)} ms`,
    empty: 'No history yet.',
    fallback: '(Tauri runtime not started) Recently copied items will appear here.',
    hints: {
      navigate: 'Navigate',
      paste: 'Paste',
      actions: 'Actions',
      settings: 'Settings',
    },
  },
  preview: {
    empty: 'Select an item to preview.',
    loading: 'Loading preview…',
    truncated: 'Preview truncated.',
    fields: {
      id: 'id',
      sensitivity: 'sensitivity',
      source: 'source',
      size: 'size',
      rank: 'rank',
    },
    none: '—',
    image: {
      loading: 'Loading image…',
      unavailable: 'Image unavailable.',
    },
  },
  status: {
    captureOn: 'Capture on',
    capturePaused: 'Capture paused',
    aiOn: 'AI on',
    aiOff: 'AI off',
    entryCount: (n) => (n === 1 ? '1 item' : `${n.toLocaleString('en')} items`),
  },
  actionMenu: {
    title: 'AI actions',
    actions: {
      Summarize: 'Summarize',
      Translate: 'Translate',
      FormatJson: 'Format JSON',
      FormatMarkdown: 'Format Markdown',
      ExplainCode: 'Explain code',
      Rewrite: 'Rewrite',
      ExtractTasks: 'Extract tasks',
      RedactSecrets: 'Redact secrets',
    },
    tauriRequired: 'AI actions require the Tauri runtime.',
    resultTitle: 'Result',
    copyResult: 'Copy',
    copied: 'Copied',
    saveResult: 'Save as new entry',
    saved: 'Saved',
    closeResult: 'Close',
    runFailed: 'AI action failed.',
  },
  onboarding: {
    title: 'Finish setting up Nagori',
    description: 'Some features need additional macOS permissions before they can run.',
    accessibilityRequired: 'Accessibility permission required',
    accessibilityHint:
      'Grant Accessibility access in System Settings → Privacy & Security so Nagori can paste into the focused app.',
    autoPasteDisabled:
      'Auto-paste is currently OFF — Enter copies to clipboard until you grant Accessibility.',
    notificationsHint: 'Allow notifications to receive AI errors and capture-paused alerts.',
    openSettings: 'Open System Settings',
    dismiss: 'Continue without it',
  },
  settings: {
    title: 'Settings',
    backToPalette: 'Back to palette',
    loading: 'Loading…',
    saving: 'Saving…',
    save: 'Save',
    tauriRequired: 'Saving settings requires the Tauri runtime.',
    tabs: {
      general: 'General',
      privacy: 'Privacy',
      ai: 'AI',
      cli: 'CLI',
      advanced: 'Advanced',
    },
    capture: {
      legend: 'Capture',
      enabled: 'Save clipboard history',
      autoPaste: 'Auto-paste on Enter',
      pasteFormatDefault: 'Default paste format',
      pasteFormatOptions: {
        preserve: 'Preserve',
        plain_text: 'Plain text',
      },
      hotkey: 'Global hotkey',
    },
    retention: {
      legend: 'Retention',
      maxCount: 'Max entries',
      maxDays: 'Retention (days)',
      maxDaysPlaceholder: '0 = unlimited',
      maxDaysHelp: 'Set to 0 to keep entries forever.',
      maxTotalBytes: 'Total storage limit',
      maxTotalBytesPlaceholder: '0 = unlimited',
      maxTotalBytesHelp: 'Pinned entries are protected even if they exceed this limit.',
      maxBytes: 'Max bytes per entry',
      pasteDelayMs: 'Paste delay (ms)',
    },
    privacy: {
      legend: 'Filters',
      localOnly: 'Local-only mode (block remote AI calls)',
      appDenylist: 'App denylist',
      appDenylistHelp: 'One source-app name per line. Captures from these apps are dropped.',
      regexDenylist: 'Regex denylist',
      regexDenylistHelp: 'One Rust regex per line. Captures matching any pattern are dropped.',
      secretHandling: 'Secret handling',
      secretHandlingHelp:
        'What to do when a clip is classified as a secret (API keys, JWTs, private keys, …).',
      secretHandlingOptions: {
        block: 'Block — refuse to store',
        store_redacted: 'Store redacted (default)',
        store_full: 'Store full (preview still redacted)',
      },
      captureKinds: 'Capture kinds',
      captureKindsHelp: 'Disabled kinds are ignored before secret classification runs.',
      captureKindOptions: {
        text: 'Text',
        url: 'URL',
        code: 'Code',
        image: 'Image',
        fileList: 'Files',
        richText: 'Rich text',
        unknown: 'Unknown',
      },
      storeFullWarning:
        "Warning: 'Store full' keeps raw API keys, JWTs, and private keys in the local SQLite DB. The DB is not encrypted at rest, so anyone with read access to your home directory (backups, sync clients, malware) can recover the secrets. Prefer 'Store redacted' unless you understand the risk.",
      storeFullConfirm:
        'Store secrets in plaintext? The DB is not encrypted; raw secrets will be recoverable from disk and from any backup that includes the data directory.',
    },
    ai: {
      legend: 'AI',
      enabled: 'Enable AI actions',
      provider: 'Provider',
      providers: {
        none: 'None',
        local: 'Local',
        remote: 'Remote',
      },
      semanticSearch: 'Enable semantic search',
    },
    cli: {
      legend: 'CLI',
      ipcEnabled: 'Allow CLI IPC connections',
    },
    appearance: {
      legend: 'Appearance',
      locale: 'Language',
      theme: 'Theme',
      themeOptions: {
        system: 'System',
        light: 'Light',
        dark: 'Dark',
      },
      recentOrder: 'Empty-query order',
      recentOrderOptions: {
        by_recency: 'Most recent',
        by_use_count: 'Most used',
        pinned_first_then_recency: 'Pinned first',
      },
    },
    integration: {
      legend: 'OS integration',
      autoLaunch: 'Launch at login',
      autoLaunchHelp: 'Register a launchd LaunchAgent so Nagori starts on login.',
    },
  },
  keybindings: {
    selectNext: 'Next result',
    selectPrev: 'Previous result',
    selectFirst: 'Jump to first',
    selectLast: 'Jump to last',
    confirm: 'Paste selection',
    openActions: 'Open AI actions',
    togglePin: 'Pin / unpin',
    delete: 'Delete',
    openSettings: 'Open settings',
    close: 'Close',
  },
  time: {
    justNow: 'just now',
    minutesAgo: (n) => (n === 1 ? '1 min ago' : `${n} min ago`),
    hoursAgo: (n) => (n === 1 ? '1 hr ago' : `${n} hr ago`),
    daysAgo: (n) => (n === 1 ? '1 day ago' : `${n} days ago`),
  },
  errors: {
    unknown: 'Unknown error.',
    storage: 'Storage error.',
    search: 'Search error.',
    platform: 'Platform error.',
    permission: 'Missing permission.',
    ai: 'AI provider error.',
    policy: 'Action blocked by policy.',
    notFound: 'Not found.',
    invalidInput: 'Invalid input.',
    unsupported: 'Unsupported on this platform.',
  },
  locales: {
    en: 'English',
    ja: '日本語',
    ko: '한국어',
    'zh-Hans': '简体中文',
  },
};

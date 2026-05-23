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
    filters: {
      toolbarLabel: string;
      today: string;
      last7days: string;
      pinned: string;
    };
  };
  preview: {
    empty: string;
    loading: string;
    truncated: string;
    truncation: {
      // Head-only fallback (legacy / tiny caps): "First 64 KB of 2.3 MB shown."
      headOnly: (parts: { shown: string; total: string }) => string;
      // Head + tail with middle elision: "First and last shown; 1.9 MB elided."
      headAndTail: (parts: { elided: string }) => string;
      // Warning shown when the active query matches text inside the elided
      // middle. Combined with `headAndTail` above it tells the user the hit
      // is real but hidden — expanding will surface it.
      elidedMatch: string;
      expand: string;
      expanding: string;
    };
    fields: {
      id: string;
      sensitivity: string;
      source: string;
      size: string;
      rank: string;
      formats: string;
    };
    none: string;
    summary: {
      lines: CountFormatter;
      // Composed image chip. Each part is pre-formatted (e.g. "1920×1080",
      // "PNG", "2.3 MB"); null entries are skipped so the locale controls
      // both the join separator and the ordering.
      image: (parts: { dimensions: string | null; format: string | null; bytes: string }) => string;
    };
    image: {
      loading: string;
      unavailable: string;
      alt: string;
    };
    fileList: {
      summary: (shown: number, total: number) => string;
      moreFiles: CountFormatter;
      // Common-parent header shown above the list when every row shares the
      // same directory prefix. The prefix string already includes the
      // trailing separator.
      inFolder: (prefix: string) => string;
    };
    url: {
      // Phishing-resistance badge surfaced when the IDN punycode form
      // differs from the displayed Unicode host. Hover/title reveals the
      // raw `xn--…` ASCII string.
      punycodeBadge: string;
      punycodeBadgeTitle: (parts: { ascii: string }) => string;
      // "Enter" kbd hint shown when external open is allowed (Public
      // entry + allowlisted scheme). Hidden otherwise so the user is
      // never told to press a key that won't work.
      openHint: string;
      // Confirm modal labels. `host` is the displayed (Unicode) host.
      confirmTitle: string;
      confirmDescription: (parts: { host: string }) => string;
      confirm: string;
      cancel: string;
      // Toast surfaced when `open_url_external` rejects the request
      // (allowlist hit, sensitivity mismatch, OS handler missing).
      openFailed: string;
    };
  };
  status: {
    captureOn: string;
    capturePaused: string;
    entryCount: CountFormatter;
    selectedCount: CountFormatter;
  };
  actionMenu: {
    title: string;
    actions: {
      Summarize: string;
      FormatJson: string;
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
    descriptionLinux: string;
    accessibilityRequired: string;
    accessibilityRequiredLinux: string;
    accessibilityHint: string;
    accessibilityHintLinux: string;
    autoPasteDisabled: string;
    autoPasteDisabledLinux: string;
    notificationsHint: string;
    openSettings: string;
    dismiss: string;
  };
  settings: {
    title: string;
    backToPalette: string;
    loading: string;
    statusSaving: string;
    statusSaved: string;
    statusError: string;
    tauriRequired: string;
    tabs: {
      general: string;
      privacy: string;
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
      captureInitialClipboard: string;
      captureInitialClipboardHelp: string;
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
      regexDenylistAutosaveHint: string;
      regexErrors: {
        lineLabel: string;
        tooLong: string;
        tooNested: string;
        invalidSyntax: string;
        empty: string;
      };
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
      menuBar: string;
      menuBarHelp: string;
      clearOnQuit: string;
      clearOnQuitHelp: string;
    };
    display: {
      legend: string;
      rowCount: string;
      rowCountHelp: string;
      previewPane: string;
      previewPaneHelp: string;
    };
    hotkeys: {
      legend: string;
      paletteHeading: string;
      paletteHelp: string;
      secondaryHeading: string;
      secondaryHelp: string;
      placeholder: string;
      paletteActions: {
        pin: string;
        delete: string;
        'paste-as-plain': string;
        'copy-without-paste': string;
        clear: string;
        'open-preview': string;
      };
      secondaryActions: {
        'repaste-last': string;
        'clear-history': string;
      };
    };
    updates: {
      legend: string;
      autoCheck: string;
      channel: string;
      checkNow: string;
      checking: string;
      upToDate: string;
      available: string;
      availableManual: string;
      viewRelease: string;
      downloadManual: string;
    };
    capabilities: {
      legend: string;
      help: string;
      platform: string;
      tier: string;
      columns: { capability: string; status: string; detail: string };
      statuses: {
        available: string;
        unsupported: string;
        requiresPermission: string;
        requiresExternalTool: string;
        experimental: string;
      };
      rows: {
        captureText: string;
        captureImage: string;
        captureFiles: string;
        writeText: string;
        writeImage: string;
        clipboardMultiRepresentationWrite: string;
        autoPaste: string;
        globalHotkey: string;
        frontmostApp: string;
        permissionsUi: string;
        updateCheck: string;
        previewQuickLook: string;
      };
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
    configuration: string;
  };
  locales: {
    system: string;
    en: string;
    ja: string;
    ko: string;
    'zh-Hans': string;
    'zh-Hant': string;
    de: string;
    fr: string;
    es: string;
  };
  toasts: {
    autoPasteFailedTitle: string;
    autoPasteFailedFallback: string;
    openSettings: string;
    dismiss: string;
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
    filters: {
      toolbarLabel: 'Quick filters',
      today: 'Today',
      last7days: 'Last 7 days',
      pinned: 'Pinned',
    },
  },
  preview: {
    empty: 'Select an item to preview.',
    loading: 'Loading preview…',
    truncated: 'Preview truncated.',
    truncation: {
      headOnly: ({ shown, total }) => `First ${shown} of ${total} shown.`,
      headAndTail: ({ elided }) => `First and last shown; middle ${elided} elided.`,
      elidedMatch: 'A search match lies inside the elided middle.',
      expand: 'Show full body',
      expanding: 'Loading full body…',
    },
    fields: {
      id: 'id',
      sensitivity: 'sensitivity',
      source: 'source',
      size: 'size',
      rank: 'rank',
      formats: 'preserved formats',
    },
    none: '—',
    summary: {
      lines: (count) => (count === 1 ? '1 line' : `${count.toLocaleString('en')} lines`),
      image: ({ dimensions, format, bytes }) =>
        [dimensions, format, bytes].filter((p): p is string => !!p).join(' · '),
    },
    image: {
      loading: 'Loading image…',
      unavailable: 'Image unavailable.',
      alt: 'Clipboard image preview',
    },
    fileList: {
      summary: (shown, total) =>
        total === shown
          ? `${total.toLocaleString('en')} ${total === 1 ? 'file' : 'files'}`
          : `${shown.toLocaleString('en')} / ${total.toLocaleString('en')} files`,
      moreFiles: (count) =>
        count === 1 ? '+1 more file' : `+${count.toLocaleString('en')} more files`,
      inFolder: (prefix) => `in ${prefix}`,
    },
    url: {
      punycodeBadge: 'punycode',
      punycodeBadgeTitle: ({ ascii }) => `IDN host. Raw ASCII form: ${ascii}`,
      openHint: 'Enter to open',
      confirmTitle: 'Open this link?',
      confirmDescription: ({ host }) => `Nagori will hand ${host} to your default browser.`,
      confirm: 'Open',
      cancel: 'Cancel',
      openFailed: 'Could not open the URL.',
    },
  },
  status: {
    captureOn: 'Capture on',
    capturePaused: 'Capture paused',
    entryCount: (n) => (n === 1 ? '1 item' : `${n.toLocaleString('en')} items`),
    selectedCount: (n) => (n === 1 ? '1 selected' : `${n.toLocaleString('en')} selected`),
  },
  actionMenu: {
    title: 'Quick actions',
    actions: {
      Summarize: 'Summarize',
      FormatJson: 'Format JSON',
      ExtractTasks: 'Extract tasks',
      RedactSecrets: 'Redact secrets',
    },
    tauriRequired: 'Quick actions require the Tauri runtime.',
    resultTitle: 'Result',
    copyResult: 'Copy',
    copied: 'Copied',
    saveResult: 'Save as new entry',
    saved: 'Saved',
    closeResult: 'Close',
    runFailed: 'Quick action failed.',
  },
  onboarding: {
    title: 'Finish setting up Nagori',
    description: 'Some features need additional macOS permissions before they can run.',
    descriptionLinux: 'Auto-paste needs an extra Linux tool before it can run.',
    accessibilityRequired: 'Accessibility permission required',
    accessibilityRequiredLinux: 'Auto-paste helper required',
    accessibilityHint:
      'Grant Accessibility access in System Settings → Privacy & Security so Nagori can paste into the focused app.',
    accessibilityHintLinux:
      'Install the `wtype` package on a Wayland session so Nagori can synthesize Ctrl+V into the focused app.',
    autoPasteDisabled:
      'Auto-paste is currently OFF — Enter copies to clipboard until you grant Accessibility.',
    autoPasteDisabledLinux:
      'Auto-paste is currently OFF — Enter copies to clipboard until `wtype` is available.',
    notificationsHint:
      'Allow notifications to receive capture-paused and auto-paste failure alerts.',
    openSettings: 'Open System Settings',
    dismiss: 'Continue without it',
  },
  settings: {
    title: 'Settings',
    backToPalette: 'Back to palette',
    loading: 'Loading…',
    statusSaving: 'Saving…',
    statusSaved: 'Saved',
    statusError: 'Save failed: {error}',
    tauriRequired: 'Saving settings requires the Tauri runtime.',
    tabs: {
      general: 'General',
      privacy: 'Privacy',
      cli: 'CLI',
      advanced: 'Advanced',
    },
    capture: {
      legend: 'Capture',
      enabled: 'Enable clipboard capture',
      autoPaste: 'Auto-paste on Enter',
      pasteFormatDefault: 'Default paste format',
      pasteFormatOptions: {
        preserve: 'Preserve',
        plain_text: 'Plain text',
      },
      hotkey: 'Global hotkey',
      captureInitialClipboard: 'Capture clipboard at launch',
      captureInitialClipboardHelp:
        'When enabled, the contents of the clipboard at startup are added to history. Disable to ignore whatever was already on the clipboard.',
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
      appDenylist: 'App denylist',
      appDenylistHelp: 'One source-app name per line. Captures from these apps are dropped.',
      regexDenylist: 'Regex denylist',
      regexDenylistHelp:
        'One pattern per line (e.g. INTERNAL-\\d+). Anything that matches is dropped before it reaches history. Keep each pattern under 256 bytes (UTF-8) and limit unescaped ( ) groups to 3 levels — split complex rules across multiple lines instead of nesting them.',
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
      regexDenylistAutosaveHint: 'Changes auto-save once the highlighted errors are fixed.',
      regexErrors: {
        lineLabel: 'Line {line}:',
        tooLong:
          'too long ({bytes} bytes > {limit}). Split it across multiple lines or drop unused alternation branches.',
        tooNested:
          'parenthesis nesting {depth} exceeds the limit of {limit}. Flatten the groups (use non-capturing (?: … ) once) or split into multiple lines.',
        invalidSyntax:
          'invalid regex syntax: {error}. Escape literal metacharacters with \\\\ or rewrite the pattern.',
        empty: 'empty entry — drop the blank line or write a pattern.',
      },
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
      recentOrder: 'History order',
      recentOrderOptions: {
        by_recency: 'Most recent',
        by_use_count: 'Most used',
        pinned_first_then_recency: 'Pinned first',
      },
    },
    integration: {
      legend: 'OS integration',
      autoLaunch: 'Launch at login',
      autoLaunchHelp:
        'Start Nagori at login using the OS-native launcher (macOS LaunchAgent, Windows Run registry key, Linux XDG autostart).',
      menuBar: 'Show tray icon',
      menuBarHelp:
        'Display the Nagori tray icon in the system tray (macOS menu bar, Windows notification area, Linux status indicator). Disable for a fully background experience.',
      clearOnQuit: 'Clear non-pinned history on quit',
      clearOnQuitHelp:
        'When the app exits, all non-pinned entries are removed. Pinned entries are preserved.',
    },
    display: {
      legend: 'Palette display',
      rowCount: 'Visible rows',
      rowCountHelp: 'Maximum number of result rows shown before scrolling (3–20).',
      previewPane: 'Show preview pane',
      previewPaneHelp: 'Hide to keep the palette compact; the result list takes the full width.',
    },
    hotkeys: {
      legend: 'Hotkeys',
      paletteHeading: 'Palette shortcuts',
      paletteHelp: 'Override the in-palette accelerators. Leave a field empty to keep the default.',
      secondaryHeading: 'Secondary global hotkeys',
      secondaryHelp:
        'Optional system-wide accelerators registered alongside the main palette hotkey.',
      placeholder: 'e.g. Cmd+Shift+P',
      paletteActions: {
        pin: 'Pin / unpin selection',
        delete: 'Delete selection',
        'paste-as-plain': 'Paste as plain text',
        'copy-without-paste': 'Copy without pasting',
        clear: 'Clear search query',
        'open-preview': 'Toggle expanded preview',
      },
      secondaryActions: {
        'repaste-last': 'Repaste most recent entry',
        'clear-history': 'Clear non-pinned history',
      },
    },
    updates: {
      legend: 'Updates',
      autoCheck: 'Check for updates automatically',
      channel: 'Channel',
      checkNow: 'Check now',
      checking: 'Checking…',
      upToDate: 'You are running the latest release.',
      available: 'Update available: {version}',
      availableManual:
        'Update available: {version}. Your install medium does not support in-app upgrade — download the new build from GitHub.',
      viewRelease: 'View release',
      downloadManual: 'Download from GitHub',
    },
    capabilities: {
      legend: 'Platform capabilities',
      help: 'What Nagori can use on your current OS. Features shown as "Needs permission" become available after you grant access in your operating system’s settings.',
      platform: 'Platform',
      tier: 'Tier',
      columns: { capability: 'Capability', status: 'Status', detail: 'Detail' },
      statuses: {
        available: 'Available',
        unsupported: 'Unsupported',
        requiresPermission: 'Needs permission',
        requiresExternalTool: 'External tool',
        experimental: 'Experimental',
      },
      rows: {
        captureText: 'Capture text',
        captureImage: 'Capture image',
        captureFiles: 'Capture files',
        writeText: 'Write text',
        writeImage: 'Write image',
        clipboardMultiRepresentationWrite: 'Multi-representation copy-back',
        autoPaste: 'Auto-paste',
        globalHotkey: 'Global hotkey',
        frontmostApp: 'Frontmost app',
        permissionsUi: 'Permissions UI',
        updateCheck: 'Update check',
        previewQuickLook: 'Preview (Quick Look)',
      },
    },
  },
  keybindings: {
    selectNext: 'Next result',
    selectPrev: 'Previous result',
    selectFirst: 'Jump to first',
    selectLast: 'Jump to last',
    confirm: 'Paste selection',
    openActions: 'Open Quick actions',
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
    ai: 'Quick action error.',
    policy: 'Action blocked by policy.',
    notFound: 'Not found.',
    invalidInput: 'Invalid input.',
    unsupported: 'Unsupported on this platform.',
    configuration: 'Configuration error. This is a build defect — please report it.',
  },
  locales: {
    system: 'System (follow OS)',
    en: 'English',
    ja: '日本語',
    ko: '한국어',
    'zh-Hans': '简体中文',
    'zh-Hant': '繁體中文',
    de: 'Deutsch',
    fr: 'Français',
    es: 'Español',
  },
  toasts: {
    autoPasteFailedTitle: 'Auto-paste failed',
    autoPasteFailedFallback: 'Auto-paste failed.',
    openSettings: 'Settings',
    dismiss: 'Dismiss',
  },
};

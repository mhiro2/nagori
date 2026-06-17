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
    // Compact badge on image result rows whose source app is a screenshot
    // tool, so "the screenshot I just took" is scannable in the list.
    screenshotBadge: string;
    hints: {
      navigate: string;
      paste: string;
      pin: string;
      actions: string;
      settings: string;
      preview: string;
    };
    filters: {
      toolbarLabel: string;
      // Date presets (single-select): clicking the active one clears it.
      today: string;
      yesterday: string;
      last7days: string;
      last30days: string;
      pinned: string;
      // Content-kind chips (multi-select). Each chip maps to exactly one
      // `ContentKind`; `richText` / `unknown` are intentionally not surfaced.
      kindText: string;
      kindUrl: string;
      kindCode: string;
      kindImage: string;
      kindFiles: string;
      // Group aria-labels for the chip sub-toolbars.
      dateGroup: string;
      typeGroup: string;
      sourceGroup: string;
      // Short trigger label for the source-app dropdown when nothing is picked.
      sourceShort: string;
      // Leading "no app filter" option in the source-app dropdown, so a
      // single-select source app can be cleared without an obscure re-click.
      allApps: string;
      // Clears every active filter (shown only when some filter is active).
      clear: string;
    };
    // Basename-first labels for `fileList` result rows.
    fileList: {
      // "+N" overflow appended after the named files when the list holds more.
      more: (overflow: number) => string;
      // Distinct-location count shown when files span more than one folder.
      locations: (count: number) => string;
      // Accessible name for a file row. `names` is the comma-joined
      // representative basenames (with any "+N"); `location` is the shared
      // folder, a location count, or null when neither applies.
      rowAria: (parts: { total: number; names: string; location: string | null }) => string;
    };
  };
  // Short labels for `RankReason` variants. Shared by the per-row reason chip
  // (ResultItem) and the full labelled list in the preview footer.
  rankReason: {
    exact: string;
    prefix: string;
    substring: string;
    fullText: string;
    fuzzy: string;
    semantic: string;
    recent: string;
    frequent: string;
    pinned: string;
  };
  preview: {
    empty: string;
    loading: string;
    // Summary label for the collapsible holding the technical fields
    // (id / sensitivity / size / rank).
    details: string;
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
    };
    // Label + coarse value categories for the "extra formats this clip kept
    // beyond its primary kind" row in the resting footer (e.g. "Additional
    // clipboard data: Image, Text"). Categories are user-facing, never raw MIME
    // types.
    additionalData: string;
    clipboardCategory: { image: string; text: string; files: string };
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
      // Visible label for the single-file "Location" row in the preview pane.
      location: string;
      // Accessible name for a file row, basename-first. `location` is the
      // parent directory (trailing separator already stripped) or null when
      // the path has no parent segment.
      fileRowAria: (parts: { name: string; location: string | null }) => string;
      // Accessible name for the supplementary thumbnail shown when the clip
      // kept an image render alongside the file list.
      thumbnailAlt: string;
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
    // Compact accessibility indicator surfaced in the palette StatusBar
    // when the OS permission required to drive auto-paste is missing. The
    // indicator is a single clickable chip that opens the Setup tab:
    //   - `autoPasteOffShort` is the visible chip label (kept short so the
    //     status bar never wraps),
    //   - `autoPasteOff` is the full sentence reused as the chip `title`,
    //   - `autoPasteOffSetupAria` is the accessible name (reason + action)
    //     so screen-reader / keyboard users get the detail the short label
    //     omits.
    autoPasteOff: string;
    autoPasteOffShort: string;
    autoPasteOffSetupAria: string;
    // Persistent auto-paste failure diagnostic. The daemon classifies why a
    // synthetic paste failed (`PasteFailureReason`); the palette leaves a chip
    // in the StatusBar so the failure outlives the toast. `label` is the chip
    // text, `hint` (keyed by the camelCase reason token) is the per-reason
    // remediation surfaced in the chip `title`, and `toolFallback` stands in
    // for a missing tool name. `accessibilityMissing` folds into the dedicated
    // accessibility chip, so its hint is only a fallback.
    pasteDiagnostics: {
      label: string;
      toolFallback: string;
      hint: {
        accessibilityMissing: string;
        toolMissing: (params: { tool: string }) => string;
        timeout: string;
        synthUnsupported: string;
        previousAppLost: string;
        unknown: string;
      };
    };
  };
  actionMenu: {
    title: string;
    // Accessible name for the panel's × dismiss control.
    close: string;
    actions: {
      SummarizeFirstSentence: string;
      FormatJson: string;
      ExtractTasks: string;
      RedactSecrets: string;
    };
    // Labels for the streaming, model-backed AI actions (one button each).
    aiActions: {
      Summarize: string;
      Rewrite: string;
      FormatMarkdown: string;
      ExtractTasks: string;
      ExplainCode: string;
    };
    // Pill shown beside AI actions in the single, unified action list.
    aiBadge: string;
    aiCancel: string;
    aiUnavailable: string;
    // Hover hint when an action can't run on the focused entry's content kind:
    // an image carries no text, and file lists / bare URLs only carry incidental
    // text (paths, the URL itself) the text actions would mangle. Keyed by the
    // kinds the action picker actually gates.
    notApplicable: {
      image: string;
      fileList: string;
      url: string;
    };
    // Localized remediation hints, keyed by the backend's `Remediation.i18n_key`.
    aiRemediation: Record<string, string>;
    tauriRequired: string;
    // Work-area status labels: a streaming AI run, a slow deterministic run,
    // and the brief completion flash.
    generating: string;
    working: string;
    done: string;
    resultTitle: string;
    copyResult: string;
    copied: string;
    saveResult: string;
    saved: string;
  };
  // The "paste as <format>" picker, surfaced from the alternate-format chord
  // when the selected entry offers more than one pasteable representation.
  pastePicker: {
    title: string;
    // The leading row: re-paste the entry as captured (every publishable
    // format at once), identical to a plain Enter.
    keepOriginal: string;
    // Row labels keyed by the representation's category token.
    categories: {
      files: string;
      image: string;
      plainText: string;
      html: string;
      richText: string;
    };
  };
  setup: {
    title: string;
    intro: string;
    accessibility: {
      title: string;
      required: string;
      description: string;
      descriptionLinux: string;
      descriptionWindows: string;
      screenshotAlt: string;
      grantButton: string;
      grantButtonRetry: string;
      recheckButton: string;
      requesting: string;
      states: {
        NotRequested: string;
        PromptShownNotGranted: string;
        Granted: string;
        RevokedAfterGranted: string;
        Unavailable: string;
      };
      statusLabel: string;
      messages: {
        NotRequested: string;
        PromptShownNotGranted: string;
        Granted: string;
        RevokedAfterGranted: string;
        UnavailableMacosFallback: string;
        UnavailableWindows: string;
        UnavailableLinux: string;
      };
      timeoutError: string;
      requestError: string;
    };
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
      setup: string;
      general: string;
      privacy: string;
      ai: string;
      cli: string;
      advanced: string;
    };
    ai: {
      legend: string;
      enabled: string;
      enabledHelp: string;
      provider: string;
      providerDisabled: string;
      providerApple: string;
      allowStreaming: string;
      allowStreamingHelp: string;
      semanticIndex: string;
      semanticIndexHelp: string;
      semanticIndexAcPowerOnly: string;
      semanticIndexAcPowerOnlyHelp: string;
      semanticIndexRebuild: string;
      semanticIndexStatus: string;
      semanticIndexStateReady: string;
      semanticIndexStateIndexing: string;
      semanticIndexStatePaused: string;
      semanticIndexStateUnavailable: string;
      semanticIndexStateUnsupported: string;
      semanticIndexStateDisabled: string;
      status: string;
      statusAvailable: string;
      statusUnavailable: string;
      statusDisabled: string;
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
      appDenylistPasswordManagers: string;
      appDenylistPasswordManagersHelp: string;
      appDenylistPatterns: string;
      appDenylistPatternsHelp: string;
      appDenylistUnsupported: string;
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
      permanentDeleteOnDelete: string;
      permanentDeleteOnDeleteHelp: string;
      purgeDeletedNow: string;
      purgeDeletedRunning: string;
      purgeDeletedDone: string;
      regexDenylistAutosaveHint: string;
      regexErrors: {
        lineLabel: string;
        tooLong: string;
        tooNested: string;
        invalidSyntax: string;
        empty: string;
      };
    };
    cli: {
      legend: string;
      ipcEnabled: string;
      install: {
        legend: string;
        help: string;
        button: string;
        reinstall: string;
        installing: string;
        statusInstalled: string;
        statusNotInstalled: string;
        installed: string;
        installedNeedsPath: string;
        notOnPath: string;
        pathExport: string;
        unavailable: string;
        unsupported: string;
      };
    };
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
      recordingHint: string;
      recordingCancelHint: string;
      clearAriaLabel: string;
      defaultMarker: string;
      disabledMarker: string;
      notSet: string;
      reset: string;
      fieldAriaLabel: string;
      restoreDefault: string;
      removeShortcut: string;
      disableShortcut: string;
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
      openSetup: string;
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
    internal: string;
    forbidden: string;
    paste: string;
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
    hotkeyRegisterFailedTitle: string;
    hotkeyRegisterFailedFallback: string;
    openSettings: string;
    dismiss: string;
    // Brief confirmation surfaced on the palette when the Accessibility
    // grant flips from not-granted to granted, so the user gets immediate
    // feedback that the Setup flow succeeded.
    accessibilityGrantedTitle: string;
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
    screenshotBadge: 'Screenshot',
    hints: {
      navigate: 'Navigate',
      paste: 'Paste',
      pin: 'Pin',
      actions: 'Actions',
      settings: 'Settings',
      preview: 'Preview',
    },
    filters: {
      toolbarLabel: 'Quick filters',
      today: 'Today',
      yesterday: 'Yesterday',
      last7days: 'Last 7 days',
      last30days: 'Last 30 days',
      pinned: 'Pinned',
      kindText: 'Text',
      kindUrl: 'URL',
      kindCode: 'Code',
      kindImage: 'Image',
      kindFiles: 'Files',
      dateGroup: 'Date',
      typeGroup: 'Type',
      sourceGroup: 'Source app',
      sourceShort: 'App',
      allApps: 'All apps',
      clear: 'Clear filters',
    },
    fileList: {
      more: (overflow) => `+${overflow.toLocaleString('en')}`,
      locations: (count) =>
        count === 1 ? '1 location' : `${count.toLocaleString('en')} locations`,
      rowAria: ({ total, names, location }) => {
        const head = total === 1 ? names : `${total.toLocaleString('en')} files: ${names}`;
        return location ? `${head}, in ${location}` : head;
      },
    },
  },
  rankReason: {
    exact: 'Exact',
    prefix: 'Prefix',
    substring: 'Match',
    fullText: 'Text',
    fuzzy: 'Fuzzy',
    semantic: 'Semantic',
    recent: 'Recent',
    frequent: 'Frequent',
    pinned: 'Pinned',
  },
  preview: {
    empty: 'Select an item to preview.',
    loading: 'Loading preview…',
    details: 'Details',
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
    },
    additionalData: 'Additional clipboard data',
    clipboardCategory: { image: 'Image', text: 'Text', files: 'Files' },
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
      location: 'Location',
      fileRowAria: ({ name, location }) => (location ? `${name}, in ${location}` : name),
      thumbnailAlt: 'Accompanying image',
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
    autoPasteOff: 'Auto-paste off — Accessibility not granted',
    autoPasteOffShort: '⚠ Auto-paste off',
    autoPasteOffSetupAria: 'Auto-paste off: Accessibility permission required. Open Setup.',
    pasteDiagnostics: {
      label: '⚠ Auto-paste failed',
      toolFallback: 'the paste tool',
      hint: {
        accessibilityMissing:
          'Auto-paste failed: Accessibility permission required. Copied — paste manually.',
        toolMissing: ({ tool }) =>
          `Auto-paste failed: ${tool} is not installed. Copied — install ${tool} or paste manually.`,
        timeout:
          'Auto-paste timed out — the compositor may be busy. Copied — paste manually or retry.',
        synthUnsupported: 'Auto-paste is not available on this platform. Copied — paste manually.',
        previousAppLost:
          'Auto-paste skipped: could not refocus the source app. Copied — paste manually.',
        unknown: 'Auto-paste failed. Copied — paste manually.',
      },
    },
  },
  actionMenu: {
    title: 'Quick actions',
    close: 'Close',
    actions: {
      SummarizeFirstSentence: 'Summarize (first sentence)',
      FormatJson: 'Format JSON',
      ExtractTasks: 'Extract tasks',
      RedactSecrets: 'Redact secrets',
    },
    aiActions: {
      Summarize: 'Summarize',
      Rewrite: 'Rewrite',
      FormatMarkdown: 'Format as Markdown',
      ExtractTasks: 'Organize tasks',
      ExplainCode: 'Explain code',
    },
    aiBadge: 'AI',
    aiCancel: 'Cancel',
    aiUnavailable: 'AI actions are unavailable right now.',
    notApplicable: {
      image: "Actions don't apply to images.",
      fileList: "Actions don't apply to files.",
      url: "This action doesn't apply to URLs.",
    },
    aiRemediation: {
      'ai.unavailable.apple_intelligence_not_enabled':
        'Enable Apple Intelligence in System Settings to use AI actions.',
      'ai.unavailable.device_not_eligible':
        'This Mac is not eligible for Apple Intelligence (Apple silicon required).',
      'ai.unavailable.model_not_ready':
        'The on-device model is still downloading. Try again shortly.',
      'ai.unavailable.asset_missing': 'A required on-device asset is unavailable.',
      'ai.unavailable.rate_limited': 'The on-device model is busy. Try again shortly.',
    },
    tauriRequired: 'Quick actions require the Tauri runtime.',
    generating: 'Generating…',
    working: 'Working…',
    done: 'Done',
    resultTitle: 'Result',
    copyResult: 'Copy',
    copied: 'Copied',
    saveResult: 'Save as new entry',
    saved: 'Saved',
  },
  pastePicker: {
    title: 'Paste as',
    keepOriginal: 'Keep original format',
    categories: {
      files: 'Files',
      image: 'Image',
      plainText: 'Plain text',
      html: 'HTML',
      richText: 'Rich text',
    },
  },
  setup: {
    title: 'Set up Nagori',
    intro:
      'Grant the permissions Nagori needs to paste into other apps. You can change these later in System Settings.',
    accessibility: {
      title: 'Accessibility',
      required: 'Required',
      description:
        'Enabling Accessibility lets Nagori paste history entries directly into the focused app. Click Grant Accessibility to open the macOS dialog, then turn the Nagori switch on.',
      descriptionLinux:
        'Install the `wtype` package on a Wayland session so Nagori can synthesize Ctrl+V into the focused app.',
      descriptionWindows:
        'On Windows, Nagori pastes into the focused app without any Accessibility-style permission — there is nothing to set up here.',
      screenshotAlt:
        'System Settings → Privacy & Security → Accessibility with the Nagori toggle highlighted.',
      grantButton: 'Grant Accessibility…',
      grantButtonRetry: 'Open System Settings',
      recheckButton: 'Re-check',
      requesting: 'Requesting…',
      states: {
        NotRequested: 'Not requested',
        PromptShownNotGranted: 'Needs action',
        Granted: 'Granted',
        RevokedAfterGranted: 'Re-enable',
        Unavailable: 'Not applicable',
      },
      statusLabel: 'Status',
      messages: {
        NotRequested:
          'Nagori has not asked macOS for Accessibility yet. Press the button below to show the system dialog.',
        PromptShownNotGranted:
          'macOS will not show the dialog a second time. Open System Settings and turn Nagori on in the Accessibility list.',
        Granted: 'Auto-paste is ready to go.',
        RevokedAfterGranted:
          'Nagori was granted Accessibility before. Re-enable it in System Settings to restore auto-paste.',
        UnavailableMacosFallback: 'Accessibility status is unavailable on this build.',
        UnavailableWindows:
          'Windows does not require an Accessibility-equivalent permission for auto-paste.',
        UnavailableLinux:
          'Auto-paste on Linux depends on the `wtype` helper. Install it through your package manager.',
      },
      timeoutError:
        'Did not detect a grant within 60 s. Open System Settings → Privacy & Security → Accessibility, verify the Nagori switch, then press Re-check.',
      requestError: 'Could not start the Accessibility request — see Console.app for details.',
    },
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
      setup: 'Setup',
      general: 'General',
      privacy: 'Privacy',
      ai: 'AI',
      cli: 'CLI',
      advanced: 'Advanced',
    },
    ai: {
      legend: 'AI',
      enabled: 'Enable AI actions',
      enabledHelp:
        'Model-backed actions like Summarize run fully on-device via Apple Intelligence. Off by default.',
      provider: 'Provider',
      providerDisabled: 'Disabled',
      providerApple: 'Apple (on-device)',
      allowStreaming: 'Stream results as they generate',
      allowStreamingHelp:
        'Show partial output while the model writes. Turn off to reveal only the final result.',
      semanticIndex: 'Semantic search index',
      semanticIndexHelp:
        'Build on-device embeddings so search can match by meaning, not just text. Uses an on-device Apple embedding model (macOS); off by default.',
      semanticIndexAcPowerOnly: 'Index only while on AC power',
      semanticIndexAcPowerOnlyHelp:
        'Pause background embedding while on battery to save power. Turn off to index on battery too.',
      semanticIndexRebuild: 'Rebuild index',
      semanticIndexStatus: 'Index status',
      semanticIndexStateReady: 'Up to date',
      semanticIndexStateIndexing: 'Indexing…',
      semanticIndexStatePaused: 'Paused (on battery)',
      semanticIndexStateUnavailable: 'Embedding model unavailable',
      semanticIndexStateUnsupported: 'Not supported on this device',
      semanticIndexStateDisabled: 'Disabled',
      status: 'Availability',
      statusAvailable: 'Available',
      statusUnavailable: 'Unavailable',
      statusDisabled: 'Disabled',
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
      appDenylistPasswordManagers: 'Block password managers',
      appDenylistPasswordManagersHelp:
        'Drops captures from bundled password managers (1Password, Bitwarden, KeePassXC, Apple Passwords) by exact app identifier. The preset is fixed; recommended on unless you actively copy from a password manager via the clipboard.',
      appDenylistPatterns: 'Custom patterns',
      appDenylistPatternsHelp:
        'One substring per line — captures whose source-app name, bundle ID, or executable path contains any of these are dropped (case-insensitive). Use this to cover apps not in the preset above, such as Dashlane, LastPass, or internal tools.',
      appDenylistUnsupported:
        'Your desktop session does not expose the frontmost app, so per-app blocking would silently match nothing. Use the regex denylist or Capture kinds below to restrict what is captured.',
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
      permanentDeleteOnDelete: 'Delete entries permanently',
      permanentDeleteOnDeleteHelp:
        'When on, deleting an entry erases it from disk immediately. When off, a deleted entry disappears from the list right away but is erased from disk a little later during routine cleanup. Secret entries are always erased immediately.',
      purgeDeletedNow: 'Purge deleted entries now',
      purgeDeletedRunning: 'Purging…',
      purgeDeletedDone: 'Purged {count} deleted entries.',
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
      install: {
        legend: 'Command-line tool',
        help: 'Install the bundled `nagori` command-line tool into ~/.local/bin so you can search and paste history from a terminal.',
        button: 'Install nagori CLI',
        reinstall: 'Reinstall',
        installing: 'Installing…',
        statusInstalled: 'nagori is linked at {path}.',
        statusNotInstalled: 'The nagori command-line tool is not installed yet.',
        installed: 'Installed nagori to {path}.',
        installedNeedsPath:
          'Installed nagori to {path}. Add the directory below to your PATH to use it.',
        notOnPath:
          '{dir} is not on your PATH yet. Add this line to your shell profile (e.g. ~/.zshrc), then open a new terminal:',
        pathExport: 'export PATH="$HOME/.local/bin:$PATH"',
        unavailable: 'The bundled CLI ships only with the packaged app, not development builds.',
        unsupported:
          'One-click install is not available on this platform. Copy the bundled nagori executable to a directory on your PATH.',
      },
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
      paletteHelp:
        'Shortcuts used within the palette. Actions you have not customized use their default keys.',
      secondaryHeading: 'Secondary global hotkeys',
      secondaryHelp:
        'Optional global shortcuts that work without opening the palette. Set only the actions you need; unset actions are disabled.',
      placeholder: 'Set shortcut',
      recordingHint: 'Press shortcut…',
      recordingCancelHint: 'Esc to cancel',
      clearAriaLabel: 'Clear shortcut',
      defaultMarker: 'Default',
      disabledMarker: 'Disabled',
      notSet: 'Not set',
      reset: 'Reset',
      fieldAriaLabel: '{action} shortcut',
      restoreDefault: 'Restore default for {action}',
      removeShortcut: 'Remove shortcut for {action}',
      disableShortcut: 'Disable {action}',
      paletteActions: {
        pin: 'Toggle pin',
        delete: 'Delete item',
        'paste-as-plain': 'Paste as…',
        'copy-without-paste': 'Copy to clipboard',
        clear: 'Clear search',
        'open-preview': 'Toggle expanded preview',
      },
      secondaryActions: {
        'repaste-last': 'Repaste latest item',
        'clear-history': 'Delete all unpinned history',
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
      openSetup: 'Open Setup',
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
    internal: 'Something went wrong. Please try again.',
    forbidden: 'Not available for this entry.',
    paste: 'Auto-paste failed. Copied — paste manually.',
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
    hotkeyRegisterFailedTitle: 'Hotkey unavailable',
    hotkeyRegisterFailedFallback: 'Failed to register the configured global hotkey.',
    openSettings: 'Settings',
    dismiss: 'Dismiss',
    accessibilityGrantedTitle: 'Accessibility granted',
  },
};

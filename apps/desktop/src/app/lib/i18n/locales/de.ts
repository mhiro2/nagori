import type { Messages } from './en';

export const de: Messages = {
  palette: {
    placeholder: 'Verlauf durchsuchen …',
    searching: 'Suchen …',
    resultCount: (count: number): string =>
      count === 1 ? '1 Ergebnis' : `${count.toLocaleString('de')} Ergebnisse`,
    elapsed: (ms: number): string => `${ms.toFixed(0)} ms`,
    empty: 'Noch kein Verlauf.',
    fallback: '(Tauri-Laufzeit nicht gestartet) Zuletzt kopierte Einträge erscheinen hier.',
    hints: {
      navigate: 'Navigieren',
      paste: 'Einfügen',
      pin: 'Anheften',
      actions: 'Aktionen',
      settings: 'Einstellungen',
    },
    filters: {
      toolbarLabel: 'Schnellfilter',
      today: 'Heute',
      yesterday: 'Gestern',
      last7days: 'Letzte 7 Tage',
      last30days: 'Letzte 30 Tage',
      pinned: 'Angeheftet',
      kindText: 'Text',
      kindUrl: 'URL',
      kindCode: 'Code',
      kindImage: 'Bild',
      kindFiles: 'Dateien',
      dateGroup: 'Datum',
      typeGroup: 'Typ',
      sourceGroup: 'Quell-App',
      sourceShort: 'App',
      allApps: 'Alle Apps',
      clear: 'Filter löschen',
    },
  },
  rankReason: {
    exact: 'Exakt',
    prefix: 'Präfix',
    substring: 'Treffer',
    fullText: 'Volltext',
    fuzzy: 'Unscharf',
    semantic: 'Semantisch',
    recent: 'Kürzlich',
    frequent: 'Häufig',
    pinned: 'Angeheftet',
  },
  preview: {
    empty: 'Eintrag auswählen, um eine Vorschau anzuzeigen.',
    loading: 'Vorschau wird geladen …',
    truncated: 'Vorschau gekürzt.',
    truncation: {
      headOnly: ({ shown, total }: { shown: string; total: string }): string =>
        `Erste ${shown} von ${total} angezeigt.`,
      headAndTail: ({ elided }: { elided: string }): string =>
        `Anfang und Ende werden angezeigt; ${elided} in der Mitte ausgelassen.`,
      elidedMatch: 'Ein Suchtreffer liegt im ausgelassenen Mittelteil.',
      expand: 'Vollständigen Inhalt anzeigen',
      expanding: 'Vollständiger Inhalt wird geladen …',
    },
    fields: {
      id: 'ID',
      sensitivity: 'Sensibilität',
      source: 'Quelle',
      size: 'Größe',
      rank: 'Rang',
      formats: 'Erhaltene Formate',
    },
    none: '—',
    summary: {
      lines: (count: number): string =>
        count === 1 ? '1 Zeile' : `${count.toLocaleString('de')} Zeilen`,
      image: ({
        dimensions,
        format,
        bytes,
      }: {
        dimensions: string | null;
        format: string | null;
        bytes: string;
      }): string => [dimensions, format, bytes].filter((p): p is string => !!p).join(' · '),
    },
    image: {
      loading: 'Bild wird geladen …',
      unavailable: 'Bild nicht verfügbar.',
      alt: 'Bildvorschau aus der Zwischenablage',
    },
    fileList: {
      summary: (shown: number, total: number): string =>
        total === shown
          ? total === 1
            ? '1 Datei'
            : `${total.toLocaleString('de')} Dateien`
          : `${shown.toLocaleString('de')} / ${total.toLocaleString('de')} Dateien`,
      moreFiles: (count: number): string =>
        count === 1 ? '+1 weitere Datei' : `+${count.toLocaleString('de')} weitere Dateien`,
      inFolder: (prefix: string): string => `in ${prefix}`,
    },
    url: {
      punycodeBadge: 'Punycode',
      punycodeBadgeTitle: ({ ascii }: { ascii: string }): string =>
        `IDN-Hostname. ASCII-Form: ${ascii}`,
      openHint: 'Mit Enter öffnen',
      confirmTitle: 'Diesen Link öffnen?',
      confirmDescription: ({ host }: { host: string }): string =>
        `Nagori öffnet ${host} im Standardbrowser.`,
      confirm: 'Öffnen',
      cancel: 'Abbrechen',
      openFailed: 'URL konnte nicht geöffnet werden.',
    },
  },
  status: {
    captureOn: 'Erfassung aktiv',
    capturePaused: 'Erfassung pausiert',
    entryCount: (n: number): string =>
      n === 1 ? '1 Eintrag' : `${n.toLocaleString('de')} Einträge`,
    selectedCount: (n: number): string =>
      n === 1 ? '1 ausgewählt' : `${n.toLocaleString('de')} ausgewählt`,
    autoPasteOff: 'Auto-Einfügen aus — Accessibility nicht erteilt',
    autoPasteOffShort: '⚠ Auto-Einfügen aus',
    autoPasteOffSetupAria:
      'Auto-Einfügen aus: Accessibility-Berechtigung erforderlich. Einrichtung öffnen.',
  },
  actionMenu: {
    title: 'Schnellaktionen',
    close: 'Schließen',
    actions: {
      SummarizeFirstSentence: 'Zusammenfassen (erster Satz)',
      FormatJson: 'JSON formatieren',
      ExtractTasks: 'Aufgaben extrahieren',
      RedactSecrets: 'Geheimnisse maskieren',
    },
    aiActions: {
      Summarize: 'Zusammenfassen',
      Rewrite: 'Umformulieren',
      FormatMarkdown: 'Als Markdown formatieren',
      ExtractTasks: 'Aufgaben ordnen',
      ExplainCode: 'Code erklären',
    },
    aiBadge: 'KI',
    aiCancel: 'Abbrechen',
    aiUnavailable: 'KI-Aktionen sind derzeit nicht verfügbar.',
    aiRemediation: {
      'ai.unavailable.apple_intelligence_not_enabled':
        'Aktiviere Apple Intelligence in den Systemeinstellungen, um KI-Aktionen zu nutzen.',
      'ai.unavailable.device_not_eligible':
        'Dieser Mac ist nicht für Apple Intelligence geeignet (Apple Silicon erforderlich).',
      'ai.unavailable.model_not_ready':
        'Das geräteseitige Modell wird noch geladen. Bitte versuche es bald erneut.',
      'ai.unavailable.asset_missing':
        'Ein erforderliches geräteseitiges Asset ist nicht verfügbar.',
      'ai.unavailable.rate_limited':
        'Das geräteseitige Modell ist ausgelastet. Bitte versuche es bald erneut.',
    },
    tauriRequired: 'Schnellaktionen erfordern die Tauri-Laufzeit.',
    generating: 'Wird generiert…',
    working: 'Wird ausgeführt…',
    done: 'Fertig',
    resultTitle: 'Ergebnis',
    copyResult: 'Kopieren',
    copied: 'Kopiert',
    saveResult: 'Als neuen Eintrag speichern',
    saved: 'Gespeichert',
  },
  setup: {
    title: 'Nagori einrichten',
    intro:
      'Erteilen Sie die Berechtigungen, die Nagori zum Einfügen in andere Apps benötigt. Sie können sie später in den Systemeinstellungen ändern.',
    accessibility: {
      title: 'Bedienungshilfen',
      required: 'Erforderlich',
      description:
        'Mit aktivierten Bedienungshilfen kann Nagori Verlaufseinträge direkt in die aktive App einfügen. Klicken Sie auf „Bedienungshilfen erteilen“, um den macOS-Dialog zu öffnen, und aktivieren Sie den Schalter für Nagori.',
      descriptionLinux:
        'Installieren Sie das Paket `wtype` in einer Wayland-Sitzung, damit Nagori Strg+V in die aktive App senden kann.',
      descriptionWindows:
        'Unter Windows fügt Nagori ohne eine Berechtigung à la Bedienungshilfen in die aktive App ein – hier ist nichts einzurichten.',
      screenshotAlt:
        'Systemeinstellungen → Datenschutz & Sicherheit → Bedienungshilfen mit hervorgehobenem Nagori-Schalter.',
      grantButton: 'Bedienungshilfen erteilen…',
      grantButtonRetry: 'Systemeinstellungen öffnen',
      recheckButton: 'Erneut prüfen',
      requesting: 'Anfrage läuft…',
      states: {
        NotRequested: 'Nicht angefragt',
        PromptShownNotGranted: 'Aktion erforderlich',
        Granted: 'Erteilt',
        RevokedAfterGranted: 'Erneut aktivieren',
        Unavailable: 'Nicht relevant',
      },
      statusLabel: 'Status',
      messages: {
        NotRequested:
          'Nagori hat macOS noch nicht um Bedienungshilfen gebeten. Klicken Sie auf die Schaltfläche, um den Systemdialog anzuzeigen.',
        PromptShownNotGranted:
          'macOS zeigt den Dialog kein zweites Mal an. Öffnen Sie die Systemeinstellungen und aktivieren Sie Nagori in der Liste der Bedienungshilfen.',
        Granted: 'Automatisches Einfügen ist einsatzbereit.',
        RevokedAfterGranted:
          'Nagori wurden zuvor Bedienungshilfen erteilt. Aktivieren Sie sie in den Systemeinstellungen erneut, um das automatische Einfügen wiederherzustellen.',
        UnavailableMacosFallback:
          'Der Status der Bedienungshilfen ist in dieser Build-Version nicht verfügbar.',
        UnavailableWindows:
          'Windows benötigt für automatisches Einfügen keine vergleichbare Berechtigung.',
        UnavailableLinux:
          'Automatisches Einfügen unter Linux benötigt das Hilfsprogramm `wtype`. Installieren Sie es über Ihren Paketmanager.',
      },
      timeoutError:
        'Innerhalb von 60 s wurde keine Freigabe erkannt. Öffnen Sie Systemeinstellungen → Datenschutz & Sicherheit → Bedienungshilfen, prüfen Sie den Nagori-Schalter und drücken Sie „Erneut prüfen“.',
      requestError:
        'Die Anfrage nach Bedienungshilfen konnte nicht gestartet werden – Details siehe Konsole.',
    },
  },
  settings: {
    title: 'Einstellungen',
    backToPalette: 'Zurück zur Palette',
    loading: 'Wird geladen …',
    statusSaving: 'Wird gespeichert …',
    statusSaved: 'Gespeichert',
    statusError: 'Speichern fehlgeschlagen: {error}',
    tauriRequired: 'Zum Speichern der Einstellungen wird die Tauri-Laufzeit benötigt.',
    tabs: {
      setup: 'Einrichtung',
      general: 'Allgemein',
      privacy: 'Datenschutz',
      ai: 'KI',
      cli: 'CLI',
      advanced: 'Erweitert',
    },
    ai: {
      legend: 'KI',
      enabled: 'KI-Aktionen aktivieren',
      enabledHelp:
        'Modellgestützte Aktionen wie Zusammenfassen laufen vollständig auf dem Gerät über Apple Intelligence. Standardmäßig aus.',
      provider: 'Anbieter',
      providerDisabled: 'Deaktiviert',
      providerApple: 'Apple (auf dem Gerät)',
      allowStreaming: 'Ergebnisse beim Erzeugen streamen',
      allowStreamingHelp:
        'Teilausgabe anzeigen, während das Modell schreibt. Ausschalten, um nur das Endergebnis zu zeigen.',
      semanticIndex: 'Semantischer Suchindex',
      semanticIndexHelp:
        'Erzeugt geräteseitige Embeddings, damit die Suche nach Bedeutung statt nur nach Text trifft. Nutzt ein geräteseitiges Apple-Embedding-Modell (macOS); standardmäßig aus.',
      semanticIndexAcPowerOnly: 'Nur bei Netzstrom indizieren',
      semanticIndexAcPowerOnlyHelp:
        'Pausiert das Embedding im Hintergrund im Akkubetrieb, um Strom zu sparen. Ausschalten, um auch im Akkubetrieb zu indizieren.',
      semanticIndexRebuild: 'Index neu aufbauen',
      semanticIndexStatus: 'Indexstatus',
      semanticIndexStateReady: 'Aktuell',
      semanticIndexStateIndexing: 'Wird indiziert …',
      semanticIndexStatePaused: 'Pausiert (Akkubetrieb)',
      semanticIndexStateUnavailable: 'Embedding-Modell nicht verfügbar',
      semanticIndexStateUnsupported: 'Auf diesem Gerät nicht unterstützt',
      semanticIndexStateDisabled: 'Deaktiviert',
      status: 'Verfügbarkeit',
      statusAvailable: 'Verfügbar',
      statusUnavailable: 'Nicht verfügbar',
      statusDisabled: 'Deaktiviert',
    },
    capture: {
      legend: 'Erfassung',
      enabled: 'Zwischenablage-Erfassung aktivieren',
      autoPaste: 'Mit Enter automatisch einfügen',
      pasteFormatDefault: 'Standard-Einfügeformat',
      pasteFormatOptions: {
        preserve: 'Format beibehalten',
        plain_text: 'Nur Text',
      },
      hotkey: 'Globaler Hotkey',
      captureInitialClipboard: 'Zwischenablage beim Start erfassen',
      captureInitialClipboardHelp:
        'Wenn aktiviert, wird der Inhalt der Zwischenablage beim Start zum Verlauf hinzugefügt. Deaktivieren, um bereits vorhandene Inhalte zu ignorieren.',
    },
    retention: {
      legend: 'Aufbewahrung',
      maxCount: 'Maximale Einträge',
      maxDays: 'Aufbewahrung (Tage)',
      maxDaysPlaceholder: '0 = unbegrenzt',
      maxDaysHelp: 'Auf 0 setzen, um Einträge unbegrenzt zu behalten.',
      maxTotalBytes: 'Gesamtspeicherlimit',
      maxTotalBytesPlaceholder: '0 = unbegrenzt',
      maxTotalBytesHelp: 'Angeheftete Einträge bleiben auch über dem Limit geschützt.',
      maxBytes: 'Max. Bytes pro Eintrag',
      pasteDelayMs: 'Einfügeverzögerung (ms)',
    },
    privacy: {
      legend: 'Filter',
      appDenylistPasswordManagers: 'Passwortmanager blockieren',
      appDenylistPasswordManagersHelp:
        'Verwirft Erfassungen aus den mitgelieferten Passwortmanagern (1Password, Bitwarden, KeePassXC, Apple Passwords) anhand exakter App-Identifier. Die Preset-Einträge sind fest und nicht editierbar. Empfohlen — lass die Option an, sofern du nicht aktiv aus einem Passwortmanager über die Zwischenablage einfügen musst.',
      appDenylistPatterns: 'Eigene Muster',
      appDenylistPatternsHelp:
        'Ein Teilstring pro Zeile — Erfassungen werden verworfen, wenn Quell-App-Name, Bundle-ID oder Programmpfad einen Eintrag enthält (case-insensitiv). Nutze diese Liste für Apps außerhalb des Presets, z. B. Dashlane, LastPass oder interne Tools.',
      appDenylistUnsupported:
        'Deine Desktop-Sitzung legt die Vordergrund-App nicht offen, daher würde App-spezifisches Blockieren stillschweigend nichts treffen. Nutze stattdessen die Regex-Sperrliste oder die Erfassungstypen unten, um Erfassungen einzuschränken.',
      regexDenylist: 'Regex-Sperrliste',
      regexDenylistHelp:
        'Ein Muster pro Zeile (z. B. INTERNAL-\\d+). Treffer landen nicht im Verlauf. Jedes Muster sollte unter 256 Byte (UTF-8) lang sein und maximal 3 Ebenen unmaskierter ( )-Klammern enthalten – komplexe Regeln bitte auf mehrere Zeilen aufteilen, statt Gruppen zu verschachteln.',
      secretHandling: 'Umgang mit Geheimnissen',
      secretHandlingHelp:
        'Was passieren soll, wenn ein Clip als Geheimnis erkannt wird (API-Schlüssel, JWTs, private Schlüssel …).',
      secretHandlingOptions: {
        block: 'Blockieren – nicht speichern',
        store_redacted: 'Geschwärzt speichern (Standard)',
        store_full: 'Vollständig speichern (Vorschau bleibt geschwärzt)',
      },
      captureKinds: 'Erfassungsarten',
      captureKindsHelp: 'Deaktivierte Arten werden vor der Geheimnis-Klassifikation ausgefiltert.',
      captureKindOptions: {
        text: 'Text',
        url: 'URL',
        code: 'Code',
        image: 'Bild',
        fileList: 'Dateien',
        richText: 'Formatierter Text',
        unknown: 'Unbekannt',
      },
      storeFullWarning:
        'Warnung: „Vollständig speichern“ behält API-Schlüssel, JWTs und private Schlüssel im lokalen SQLite-DB. Die Datenbank ist nicht im Ruhezustand verschlüsselt – jeder mit Lesezugriff auf Ihr Home-Verzeichnis (Backups, Sync-Clients, Schadsoftware) kann die Geheimnisse wiederherstellen. Wählen Sie „Geschwärzt speichern“, sofern Sie das Risiko nicht eingeschätzt haben.',
      storeFullConfirm:
        'Geheimnisse im Klartext speichern? Die Datenbank ist unverschlüsselt; rohe Geheimnisse sind von der Festplatte und aus jedem Backup mit dem Datenverzeichnis rekonstruierbar.',
      regexDenylistAutosaveHint:
        'Sobald die markierten Regex-Fehler behoben sind, wird automatisch gespeichert.',
      regexErrors: {
        lineLabel: 'Zeile {line}:',
        tooLong:
          'zu lang ({bytes} Bytes > {limit}). Teilen Sie das Muster auf mehrere Zeilen auf oder entfernen Sie nicht benötigte Alternativen.',
        tooNested:
          'Klammerverschachtelung {depth} überschreitet das Limit von {limit}. Flachen Sie die Gruppen ab (z. B. einmal (?: … ) verwenden) oder teilen Sie das Muster auf.',
        invalidSyntax:
          'ungültige Regex-Syntax: {error}. Maskieren Sie wörtliche Metazeichen mit \\\\ oder schreiben Sie das Muster um.',
        empty: 'leerer Eintrag – entfernen Sie die leere Zeile oder schreiben Sie ein Muster.',
      },
    },
    cli: {
      legend: 'CLI',
      ipcEnabled: 'CLI-IPC-Verbindungen erlauben',
      install: {
        legend: 'Kommandozeilen-Tool',
        help: 'Installiere das mitgelieferte `nagori`-Kommandozeilen-Tool nach ~/.local/bin, um den Verlauf vom Terminal aus zu durchsuchen und einzufügen.',
        button: 'nagori-CLI installieren',
        reinstall: 'Neu installieren',
        installing: 'Wird installiert…',
        statusInstalled: 'nagori ist unter {path} verknüpft.',
        statusNotInstalled: 'Das nagori-Kommandozeilen-Tool ist noch nicht installiert.',
        installed: 'nagori wurde nach {path} installiert.',
        installedNeedsPath:
          'nagori wurde nach {path} installiert. Füge das untenstehende Verzeichnis zu deinem PATH hinzu, um es zu nutzen.',
        notOnPath:
          '{dir} ist noch nicht in deinem PATH. Füge diese Zeile zu deinem Shell-Profil hinzu (z. B. ~/.zshrc) und öffne ein neues Terminal:',
        pathExport: 'export PATH="$HOME/.local/bin:$PATH"',
        unavailable:
          'Das mitgelieferte CLI ist nur in der paketierten App enthalten, nicht in Entwicklungs-Builds.',
        unsupported:
          'Die Ein-Klick-Installation ist auf dieser Plattform nicht verfügbar. Kopiere die mitgelieferte nagori-Datei in ein Verzeichnis in deinem PATH.',
      },
    },
    appearance: {
      legend: 'Darstellung',
      locale: 'Sprache',
      theme: 'Erscheinungsbild',
      themeOptions: {
        system: 'System',
        light: 'Hell',
        dark: 'Dunkel',
      },
      recentOrder: 'Verlaufsreihenfolge',
      recentOrderOptions: {
        by_recency: 'Neueste zuerst',
        by_use_count: 'Häufigste zuerst',
        pinned_first_then_recency: 'Angeheftete zuerst',
      },
    },
    integration: {
      legend: 'OS-Integration',
      autoLaunch: 'Bei Anmeldung starten',
      autoLaunchHelp:
        'Startet Nagori bei der Anmeldung über das OS-eigene Verfahren (macOS LaunchAgent, Windows Run-Registry-Eintrag, Linux XDG-Autostart).',
      menuBar: 'Tray-Symbol anzeigen',
      menuBarHelp:
        'Zeigt das Nagori-Tray-Symbol im System-Tray an (macOS: Menüleiste, Windows: Infobereich, Linux: Statusanzeige). Deaktivieren für reinen Hintergrundbetrieb.',
      clearOnQuit: 'Nicht angeheftete Einträge beim Beenden löschen',
      clearOnQuitHelp:
        'Beim Beenden werden alle nicht angehefteten Einträge entfernt. Angeheftete Einträge bleiben erhalten.',
    },
    display: {
      legend: 'Paletten-Anzeige',
      rowCount: 'Sichtbare Zeilen',
      rowCountHelp: 'Maximale Anzahl an Ergebniszeilen vor dem Scrollen (3–20).',
      previewPane: 'Vorschaubereich anzeigen',
      previewPaneHelp:
        'Ausblenden, um die Palette kompakt zu halten; die Liste nutzt die volle Breite.',
    },
    hotkeys: {
      legend: 'Tastenkürzel',
      paletteHeading: 'Paletten-Tastenkürzel',
      paletteHelp:
        'Überschreibt die Tastenkürzel innerhalb der Palette. Leeres Feld bewahrt die Voreinstellung.',
      secondaryHeading: 'Zusätzliche globale Tastenkürzel',
      secondaryHelp:
        'Optionale systemweite Tastenkürzel, die neben dem Haupt-Hotkey der Palette registriert werden.',
      placeholder: 'Klicken zum Aufzeichnen',
      recordingHint: 'Tastenkürzel drücken… (Esc zum Abbrechen)',
      clearAriaLabel: 'Tastenkürzel löschen',
      paletteActions: {
        pin: 'Auswahl anheften / lösen',
        delete: 'Auswahl löschen',
        'paste-as-plain': 'Als reinen Text einfügen',
        'copy-without-paste': 'Nur kopieren, nicht einfügen',
        clear: 'Suchanfrage leeren',
        'open-preview': 'Erweiterte Vorschau umschalten',
      },
      secondaryActions: {
        'repaste-last': 'Letzten Eintrag erneut einfügen',
        'clear-history': 'Nicht angeheftete Einträge löschen',
      },
    },
    updates: {
      legend: 'Updates',
      autoCheck: 'Automatisch nach Updates suchen',
      channel: 'Kanal',
      checkNow: 'Jetzt prüfen',
      checking: 'Wird geprüft …',
      upToDate: 'Sie verwenden die neueste Version.',
      available: 'Update verfügbar: {version}',
      availableManual:
        'Update verfügbar: {version}. Die Installationsart unterstützt kein In-place-Update — bitte den neuen Build von GitHub herunterladen.',
      viewRelease: 'Release anzeigen',
      downloadManual: 'Von GitHub herunterladen',
    },
    capabilities: {
      legend: 'Plattformfähigkeiten',
      help: 'Was Nagori auf deinem aktuellen Betriebssystem nutzen kann. Funktionen mit dem Status „Berechtigung erforderlich" werden verfügbar, sobald du in den Systemeinstellungen deines Betriebssystems den Zugriff erlaubst.',
      platform: 'Plattform',
      tier: 'Stufe',
      openSetup: 'Einrichtung öffnen',
      columns: { capability: 'Fähigkeit', status: 'Status', detail: 'Details' },
      statuses: {
        available: 'Verfügbar',
        unsupported: 'Nicht unterstützt',
        requiresPermission: 'Berechtigung erforderlich',
        requiresExternalTool: 'Externes Werkzeug',
        experimental: 'Experimentell',
      },
      rows: {
        captureText: 'Text erfassen',
        captureImage: 'Bild erfassen',
        captureFiles: 'Dateien erfassen',
        writeText: 'Text schreiben',
        writeImage: 'Bild schreiben',
        clipboardMultiRepresentationWrite: 'Mehrfachdarstellung beim Zurückschreiben',
        autoPaste: 'Automatisches Einfügen',
        globalHotkey: 'Globales Tastenkürzel',
        frontmostApp: 'Vordergrund-App',
        permissionsUi: 'Berechtigungs-UI',
        updateCheck: 'Updateprüfung',
        previewQuickLook: 'Vorschau (Quick Look)',
      },
    },
  },
  keybindings: {
    selectNext: 'Nächstes Ergebnis',
    selectPrev: 'Vorheriges Ergebnis',
    selectFirst: 'Zum ersten springen',
    selectLast: 'Zum letzten springen',
    confirm: 'Auswahl einfügen',
    openActions: 'Schnellaktionen öffnen',
    togglePin: 'Anheften / lösen',
    delete: 'Löschen',
    openSettings: 'Einstellungen öffnen',
    close: 'Schließen',
  },
  time: {
    justNow: 'gerade eben',
    minutesAgo: (n: number): string => (n === 1 ? 'vor 1 Min.' : `vor ${n} Min.`),
    hoursAgo: (n: number): string => (n === 1 ? 'vor 1 Std.' : `vor ${n} Std.`),
    daysAgo: (n: number): string => (n === 1 ? 'vor 1 Tag' : `vor ${n} Tagen`),
  },
  errors: {
    unknown: 'Unbekannter Fehler.',
    storage: 'Speicherfehler.',
    search: 'Suchfehler.',
    platform: 'Plattformfehler.',
    permission: 'Fehlende Berechtigung.',
    ai: 'Fehler bei Schnellaktion.',
    policy: 'Aktion durch Richtlinie blockiert.',
    notFound: 'Nicht gefunden.',
    invalidInput: 'Ungültige Eingabe.',
    unsupported: 'Auf dieser Plattform nicht unterstützt.',
    configuration: 'Konfigurationsfehler. Dies ist ein Build-Defekt — bitte melden.',
  },
  locales: {
    system: 'System (Betriebssystem folgen)',
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
    autoPasteFailedTitle: 'Automatisches Einfügen fehlgeschlagen',
    autoPasteFailedFallback: 'Automatisches Einfügen fehlgeschlagen.',
    hotkeyRegisterFailedTitle: 'Tastenkürzel nicht verfügbar',
    hotkeyRegisterFailedFallback:
      'Registrierung des konfigurierten globalen Tastenkürzels fehlgeschlagen.',
    openSettings: 'Einstellungen',
    dismiss: 'Schließen',
    accessibilityGrantedTitle: 'Accessibility erteilt',
  },
};

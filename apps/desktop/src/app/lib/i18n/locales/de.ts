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
      actions: 'Aktionen',
      settings: 'Einstellungen',
    },
    filters: {
      toolbarLabel: 'Schnellfilter',
      today: 'Heute',
      last7days: 'Letzte 7 Tage',
      pinned: 'Angeheftet',
    },
  },
  preview: {
    empty: 'Eintrag auswählen, um eine Vorschau anzuzeigen.',
    loading: 'Vorschau wird geladen …',
    truncated: 'Vorschau gekürzt.',
    fields: {
      id: 'ID',
      sensitivity: 'Sensibilität',
      source: 'Quelle',
      size: 'Größe',
      rank: 'Rang',
    },
    none: '—',
    image: {
      loading: 'Bild wird geladen …',
      unavailable: 'Bild nicht verfügbar.',
    },
  },
  status: {
    captureOn: 'Erfassung aktiv',
    capturePaused: 'Erfassung pausiert',
    aiOn: 'KI aktiv',
    aiOff: 'KI aus',
    entryCount: (n: number): string =>
      n === 1 ? '1 Eintrag' : `${n.toLocaleString('de')} Einträge`,
    selectedCount: (n: number): string =>
      n === 1 ? '1 ausgewählt' : `${n.toLocaleString('de')} ausgewählt`,
  },
  actionMenu: {
    title: 'KI-Aktionen',
    actions: {
      Summarize: 'Zusammenfassen',
      Translate: 'Übersetzen',
      FormatJson: 'JSON formatieren',
      FormatMarkdown: 'Markdown formatieren',
      ExplainCode: 'Code erklären',
      Rewrite: 'Umschreiben',
      ExtractTasks: 'Aufgaben extrahieren',
      RedactSecrets: 'Geheimnisse maskieren',
    },
    tauriRequired: 'KI-Aktionen erfordern die Tauri-Laufzeit.',
    resultTitle: 'Ergebnis',
    copyResult: 'Kopieren',
    copied: 'Kopiert',
    saveResult: 'Als neuen Eintrag speichern',
    saved: 'Gespeichert',
    closeResult: 'Schließen',
    runFailed: 'KI-Aktion fehlgeschlagen.',
  },
  onboarding: {
    title: 'Nagori-Einrichtung abschließen',
    description: 'Einige Funktionen benötigen zusätzliche macOS-Berechtigungen.',
    accessibilityRequired: 'Berechtigung „Bedienungshilfen“ erforderlich',
    accessibilityHint:
      'Erteilen Sie unter „Systemeinstellungen → Datenschutz & Sicherheit → Bedienungshilfen“ Zugriff für Nagori, damit es in die aktive App einfügen kann.',
    autoPasteDisabled:
      'Automatisches Einfügen ist derzeit AUS – Enter kopiert nur in die Zwischenablage, bis die Bedienungshilfen freigegeben sind.',
    notificationsHint:
      'Mitteilungen erlauben, um KI-Fehler und Hinweise zu pausierter Erfassung zu erhalten.',
    openSettings: 'Systemeinstellungen öffnen',
    dismiss: 'Vorerst ohne fortfahren',
  },
  settings: {
    title: 'Einstellungen',
    backToPalette: 'Zurück zur Palette',
    loading: 'Wird geladen …',
    saving: 'Wird gespeichert …',
    save: 'Speichern',
    tauriRequired: 'Zum Speichern der Einstellungen wird die Tauri-Laufzeit benötigt.',
    tabs: {
      general: 'Allgemein',
      privacy: 'Datenschutz',
      ai: 'KI',
      cli: 'CLI',
      advanced: 'Erweitert',
    },
    capture: {
      legend: 'Erfassung',
      enabled: 'Zwischenablage-Verlauf speichern',
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
      localOnly: 'Nur-Lokal-Modus (Remote-KI-Aufrufe blockieren)',
      appDenylist: 'App-Sperrliste',
      appDenylistHelp:
        'Ein Quell-App-Name pro Zeile. Erfassungen aus diesen Apps werden verworfen.',
      regexDenylist: 'Regex-Sperrliste',
      regexDenylistHelp:
        'Ein Rust-Regex pro Zeile. Erfassungen, die einem Muster entsprechen, werden verworfen.',
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
    },
    ai: {
      legend: 'KI',
      enabled: 'KI-Aktionen aktivieren',
      provider: 'Anbieter',
      providers: {
        none: 'Keiner',
        local: 'Lokal',
        remote: 'Remote',
      },
      semanticSearch: 'Semantische Suche aktivieren',
    },
    cli: {
      legend: 'CLI',
      ipcEnabled: 'CLI-IPC-Verbindungen erlauben',
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
      recentOrder: 'Reihenfolge bei leerer Suche',
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
      placeholder: 'z. B. Cmd+Shift+P',
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
      autoCheckHelp:
        'Prüft regelmäßig den Release-Feed und zeigt ein Banner, sobald ein neuer Build verfügbar ist. Der Download wird niemals ohne Ihre Bestätigung installiert.',
      autoCheckLocalOnly:
        'Deaktiviert, solange der Nur-Lokal-Modus aktiv ist. Schalten Sie ihn unter Datenschutz → Nur-Lokal-Modus aus, um Update-Prüfungen zuzulassen.',
      channel: 'Kanal',
      checkNow: 'Jetzt nach Updates suchen',
      checking: 'Wird geprüft …',
      upToDate: 'Sie verwenden die neueste Version.',
      available: 'Update verfügbar: {version}',
      viewRelease: 'Release anzeigen',
    },
  },
  keybindings: {
    selectNext: 'Nächstes Ergebnis',
    selectPrev: 'Vorheriges Ergebnis',
    selectFirst: 'Zum ersten springen',
    selectLast: 'Zum letzten springen',
    confirm: 'Auswahl einfügen',
    openActions: 'KI-Aktionen öffnen',
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
    ai: 'Fehler beim KI-Anbieter.',
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
    openSettings: 'Einstellungen',
    dismiss: 'Schließen',
  },
};

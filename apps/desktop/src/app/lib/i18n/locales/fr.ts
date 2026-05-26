import type { Messages } from './en';

export const fr: Messages = {
  palette: {
    placeholder: 'Rechercher dans l’historique…',
    searching: 'Recherche…',
    resultCount: (count: number): string =>
      count <= 1 ? `${count} résultat` : `${count.toLocaleString('fr')} résultats`,
    elapsed: (ms: number): string => `${ms.toFixed(0)} ms`,
    empty: 'Aucun historique pour le moment.',
    fallback: '(Runtime Tauri non démarré) Les éléments récemment copiés apparaîtront ici.',
    hints: {
      navigate: 'Naviguer',
      paste: 'Coller',
      actions: 'Actions',
      settings: 'Paramètres',
    },
    filters: {
      toolbarLabel: 'Filtres rapides',
      today: 'Aujourd’hui',
      last7days: '7 derniers jours',
      pinned: 'Épinglés',
    },
  },
  preview: {
    empty: 'Sélectionnez un élément à prévisualiser.',
    loading: 'Chargement de l’aperçu…',
    truncated: 'Aperçu tronqué.',
    truncation: {
      headOnly: ({ shown, total }: { shown: string; total: string }): string =>
        `Affichage des ${shown} premiers sur ${total}.`,
      headAndTail: ({ elided }: { elided: string }): string =>
        `Début et fin affichés ; ${elided} omis au milieu.`,
      elidedMatch: 'Une correspondance de recherche se trouve dans la partie omise.',
      expand: 'Afficher tout le contenu',
      expanding: 'Chargement du contenu complet…',
    },
    fields: {
      id: 'id',
      sensitivity: 'sensibilité',
      source: 'source',
      size: 'taille',
      rank: 'rang',
      formats: 'formats conservés',
    },
    none: '—',
    summary: {
      lines: (count: number): string =>
        count <= 1 ? `${count} ligne` : `${count.toLocaleString('fr')} lignes`,
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
      loading: 'Chargement de l’image…',
      unavailable: 'Image indisponible.',
      alt: 'Aperçu de l’image du presse-papiers',
    },
    fileList: {
      summary: (shown: number, total: number): string =>
        total === shown
          ? total <= 1
            ? `${total} fichier`
            : `${total.toLocaleString('fr')} fichiers`
          : `${shown.toLocaleString('fr')} / ${total.toLocaleString('fr')} fichiers`,
      moreFiles: (count: number): string =>
        count <= 1
          ? `+${count} fichier de plus`
          : `+${count.toLocaleString('fr')} fichiers de plus`,
      inFolder: (prefix: string): string => `dans ${prefix}`,
    },
    url: {
      punycodeBadge: 'punycode',
      punycodeBadgeTitle: ({ ascii }: { ascii: string }): string =>
        `Hôte IDN. Forme ASCII : ${ascii}`,
      openHint: 'Entrée pour ouvrir',
      confirmTitle: 'Ouvrir ce lien ?',
      confirmDescription: ({ host }: { host: string }): string =>
        `Nagori va ouvrir ${host} dans votre navigateur par défaut.`,
      confirm: 'Ouvrir',
      cancel: 'Annuler',
      openFailed: 'Impossible d’ouvrir l’URL.',
    },
  },
  status: {
    captureOn: 'Capture active',
    capturePaused: 'Capture en pause',
    entryCount: (n: number): string =>
      n <= 1 ? `${n} élément` : `${n.toLocaleString('fr')} éléments`,
    selectedCount: (n: number): string =>
      n <= 1 ? `${n} sélectionné` : `${n.toLocaleString('fr')} sélectionnés`,
  },
  actionMenu: {
    title: 'Actions rapides',
    actions: {
      Summarize: 'Résumer',
      FormatJson: 'Formater JSON',
      ExtractTasks: 'Extraire les tâches',
      RedactSecrets: 'Masquer les secrets',
    },
    tauriRequired: 'Les actions rapides nécessitent le runtime Tauri.',
    resultTitle: 'Résultat',
    copyResult: 'Copier',
    copied: 'Copié',
    saveResult: 'Enregistrer comme nouvelle entrée',
    saved: 'Enregistré',
    closeResult: 'Fermer',
    runFailed: 'Échec de l’action rapide.',
  },
  onboarding: {
    title: 'Terminer la configuration de Nagori',
    description: 'Certaines fonctionnalités nécessitent des autorisations macOS supplémentaires.',
    descriptionLinux: 'Le collage automatique requiert un outil Linux supplémentaire.',
    accessibilityRequired: 'Autorisation d’accessibilité requise',
    accessibilityRequiredLinux: 'Outil de collage automatique requis',
    accessibilityHint:
      'Accordez l’accès à Accessibilité dans Réglages Système → Confidentialité et sécurité pour que Nagori puisse coller dans l’application active.',
    accessibilityHintLinux:
      'Installez le paquet `wtype` dans une session Wayland pour que Nagori puisse synthétiser Ctrl+V dans l’application active.',
    autoPasteDisabled:
      'Le collage automatique est désactivé — Entrée se contente de copier dans le presse-papiers tant que l’Accessibilité n’est pas accordée.',
    autoPasteDisabledLinux:
      'Le collage automatique est désactivé — Entrée se contente de copier dans le presse-papiers tant que `wtype` n’est pas disponible.',
    notificationsHint:
      'Autorisez les notifications pour recevoir les alertes de capture en pause et d’échec de collage automatique.',
    openSettings: 'Ouvrir les Réglages Système',
    dismiss: 'Continuer sans',
  },
  settings: {
    title: 'Paramètres',
    backToPalette: 'Retour à la palette',
    loading: 'Chargement…',
    statusSaving: 'Enregistrement…',
    statusSaved: 'Enregistré',
    statusError: 'Échec de l’enregistrement : {error}',
    tauriRequired: 'L’enregistrement des paramètres nécessite le runtime Tauri.',
    tabs: {
      general: 'Général',
      privacy: 'Confidentialité',
      cli: 'CLI',
      advanced: 'Avancé',
    },
    capture: {
      legend: 'Capture',
      enabled: 'Activer la capture du presse-papiers',
      autoPaste: 'Coller automatiquement avec Entrée',
      pasteFormatDefault: 'Format de collage par défaut',
      pasteFormatOptions: {
        preserve: 'Conserver',
        plain_text: 'Texte brut',
      },
      hotkey: 'Raccourci global',
      captureInitialClipboard: 'Capturer le presse-papiers au démarrage',
      captureInitialClipboardHelp:
        'Lorsqu’activé, le contenu du presse-papiers au démarrage est ajouté à l’historique. Désactivez pour ignorer ce qui s’y trouvait déjà.',
    },
    retention: {
      legend: 'Conservation',
      maxCount: 'Nombre maximum d’entrées',
      maxDays: 'Conservation (jours)',
      maxDaysPlaceholder: '0 = illimité',
      maxDaysHelp: 'Définir à 0 pour conserver les entrées indéfiniment.',
      maxTotalBytes: 'Limite de stockage totale',
      maxTotalBytesPlaceholder: '0 = illimité',
      maxTotalBytesHelp: 'Les entrées épinglées sont protégées même si elles dépassent la limite.',
      maxBytes: 'Octets max. par entrée',
      pasteDelayMs: 'Délai de collage (ms)',
    },
    privacy: {
      legend: 'Filtres',
      appDenylist: 'Liste de refus d’apps',
      appDenylistHelp:
        'Un nom d’application source par ligne. Les captures depuis ces apps sont ignorées.',
      regexDenylist: 'Liste de refus regex',
      regexDenylistHelp:
        'Un motif par ligne (ex. INTERNAL-\\d+). Toute correspondance est ignorée avant d’atteindre l’historique. Chaque motif doit faire moins de 256 octets (UTF-8) et ne pas dépasser 3 niveaux de parenthèses ( ) non échappées ; scindez les règles complexes sur plusieurs lignes plutôt que d’imbriquer des groupes.',
      secretHandling: 'Gestion des secrets',
      secretHandlingHelp:
        'Que faire lorsqu’un clip est classé comme secret (clés API, JWT, clés privées, …).',
      secretHandlingOptions: {
        block: 'Bloquer — ne pas enregistrer',
        store_redacted: 'Enregistrer masqué (par défaut)',
        store_full: 'Enregistrer en entier (l’aperçu reste masqué)',
      },
      captureKinds: 'Types de capture',
      captureKindsHelp: 'Les types désactivés sont ignorés avant la classification des secrets.',
      captureKindOptions: {
        text: 'Texte',
        url: 'URL',
        code: 'Code',
        image: 'Image',
        fileList: 'Fichiers',
        richText: 'Texte enrichi',
        unknown: 'Inconnu',
      },
      storeFullWarning:
        'Avertissement : « Enregistrer en entier » conserve les clés API, JWT et clés privées en clair dans la base SQLite locale. La base n’est pas chiffrée au repos, donc quiconque a accès en lecture à votre dossier personnel (sauvegardes, clients de synchronisation, logiciels malveillants) peut récupérer les secrets. Préférez « Enregistrer masqué » si vous ne mesurez pas le risque.',
      storeFullConfirm:
        'Enregistrer les secrets en clair ? La base n’est pas chiffrée ; les secrets bruts seront récupérables depuis le disque et depuis toute sauvegarde incluant le répertoire de données.',
      regexDenylistAutosaveHint:
        'Les modifications sont enregistrées automatiquement une fois les erreurs regex corrigées.',
      regexErrors: {
        lineLabel: 'Ligne {line} :',
        tooLong:
          'trop long ({bytes} octets > {limit}). Scindez le motif sur plusieurs lignes ou supprimez les branches inutiles.',
        tooNested:
          'imbrication de parenthèses {depth} au-delà de la limite {limit}. Aplatissez les groupes (utilisez une seule fois (?: … )) ou scindez sur plusieurs lignes.',
        invalidSyntax:
          'syntaxe regex invalide : {error}. Échappez les métacaractères littéraux avec \\\\ ou réécrivez le motif.',
        empty: 'entrée vide — supprimez la ligne vide ou saisissez un motif.',
      },
    },
    cli: {
      legend: 'CLI',
      ipcEnabled: 'Autoriser les connexions IPC du CLI',
    },
    appearance: {
      legend: 'Apparence',
      locale: 'Langue',
      theme: 'Thème',
      themeOptions: {
        system: 'Système',
        light: 'Clair',
        dark: 'Sombre',
      },
      recentOrder: 'Ordre de l’historique',
      recentOrderOptions: {
        by_recency: 'Plus récents',
        by_use_count: 'Plus utilisés',
        pinned_first_then_recency: 'Épinglés d’abord',
      },
    },
    integration: {
      legend: 'Intégration OS',
      autoLaunch: 'Lancer à la connexion',
      autoLaunchHelp:
        'Démarre Nagori à la connexion via le mécanisme natif du système (LaunchAgent sur macOS, clé Run du registre sur Windows, autostart XDG sur Linux).',
      menuBar: 'Afficher l’icône dans la barre d’état',
      menuBarHelp:
        'Affiche l’icône de Nagori dans la barre d’état système (macOS : barre des menus, Windows : zone de notification, Linux : indicateur d’état). Désactivez pour une exécution entièrement en arrière-plan.',
      clearOnQuit: 'Effacer l’historique non épinglé à la fermeture',
      clearOnQuitHelp:
        'À la fermeture de l’application, toutes les entrées non épinglées sont supprimées. Les épinglées sont conservées.',
    },
    display: {
      legend: 'Affichage de la palette',
      rowCount: 'Lignes visibles',
      rowCountHelp: 'Nombre maximum de lignes de résultats avant le défilement (3–20).',
      previewPane: 'Afficher le volet d’aperçu',
      previewPaneHelp: 'Masquer pour garder la palette compacte ; la liste prend toute la largeur.',
    },
    hotkeys: {
      legend: 'Raccourcis',
      paletteHeading: 'Raccourcis de la palette',
      paletteHelp:
        'Remplacez les raccourcis internes à la palette. Laissez vide pour conserver le réglage par défaut.',
      secondaryHeading: 'Raccourcis globaux secondaires',
      secondaryHelp:
        'Raccourcis système optionnels enregistrés en parallèle du raccourci principal de la palette.',
      placeholder: 'Cliquer pour enregistrer',
      recordingHint: 'Appuyez sur la combinaison… (Échap pour annuler)',
      clearAriaLabel: 'Effacer le raccourci',
      paletteActions: {
        pin: 'Épingler / désépingler',
        delete: 'Supprimer la sélection',
        'paste-as-plain': 'Coller en texte brut',
        'copy-without-paste': 'Copier sans coller',
        clear: 'Effacer la requête',
        'open-preview': 'Basculer l’aperçu étendu',
      },
      secondaryActions: {
        'repaste-last': 'Recoller l’entrée la plus récente',
        'clear-history': 'Effacer l’historique non épinglé',
      },
    },
    updates: {
      legend: 'Mises à jour',
      autoCheck: 'Vérifier automatiquement les mises à jour',
      channel: 'Canal',
      checkNow: 'Vérifier',
      checking: 'Vérification…',
      upToDate: 'Vous utilisez la dernière version.',
      available: 'Mise à jour disponible : {version}',
      availableManual:
        'Mise à jour disponible : {version}. Votre mode d’installation ne permet pas la mise à jour en place — téléchargez la nouvelle build depuis GitHub.',
      viewRelease: 'Voir la version',
      downloadManual: 'Télécharger depuis GitHub',
    },
    capabilities: {
      legend: 'Capacités de la plateforme',
      help: 'Ce que Nagori peut utiliser sur votre système d’exploitation actuel. Les fonctionnalités marquées « Autorisation requise » deviennent disponibles une fois l’accès accordé dans les réglages système de votre OS.',
      platform: 'Plateforme',
      tier: 'Niveau',
      columns: { capability: 'Capacité', status: 'Statut', detail: 'Détail' },
      statuses: {
        available: 'Disponible',
        unsupported: 'Non pris en charge',
        requiresPermission: 'Autorisation requise',
        requiresExternalTool: 'Outil externe',
        experimental: 'Expérimental',
      },
      rows: {
        captureText: 'Capturer le texte',
        captureImage: 'Capturer l’image',
        captureFiles: 'Capturer les fichiers',
        writeText: 'Écrire le texte',
        writeImage: 'Écrire l’image',
        clipboardMultiRepresentationWrite: 'Réécriture multi-représentation',
        autoPaste: 'Collage automatique',
        globalHotkey: 'Raccourci global',
        frontmostApp: 'Application au premier plan',
        permissionsUi: 'Interface des autorisations',
        updateCheck: 'Vérification des mises à jour',
        previewQuickLook: 'Aperçu (Quick Look)',
      },
    },
  },
  keybindings: {
    selectNext: 'Résultat suivant',
    selectPrev: 'Résultat précédent',
    selectFirst: 'Aller au premier',
    selectLast: 'Aller au dernier',
    confirm: 'Coller la sélection',
    openActions: 'Ouvrir les actions rapides',
    togglePin: 'Épingler / désépingler',
    delete: 'Supprimer',
    openSettings: 'Ouvrir les paramètres',
    close: 'Fermer',
  },
  time: {
    justNow: 'à l’instant',
    minutesAgo: (n: number): string => (n <= 1 ? 'il y a 1 min' : `il y a ${n} min`),
    hoursAgo: (n: number): string => (n <= 1 ? 'il y a 1 h' : `il y a ${n} h`),
    daysAgo: (n: number): string => (n <= 1 ? 'il y a 1 j' : `il y a ${n} j`),
  },
  errors: {
    unknown: 'Erreur inconnue.',
    storage: 'Erreur de stockage.',
    search: 'Erreur de recherche.',
    platform: 'Erreur de plateforme.',
    permission: 'Autorisation manquante.',
    ai: 'Erreur d’action rapide.',
    policy: 'Action bloquée par la stratégie.',
    notFound: 'Introuvable.',
    invalidInput: 'Entrée invalide.',
    unsupported: 'Non pris en charge sur cette plateforme.',
    configuration:
      'Erreur de configuration. Il s’agit d’un défaut de build — veuillez le signaler.',
  },
  locales: {
    system: 'Système (suivre l’OS)',
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
    autoPasteFailedTitle: 'Échec du collage automatique',
    autoPasteFailedFallback: 'Échec du collage automatique.',
    hotkeyRegisterFailedTitle: 'Raccourci indisponible',
    hotkeyRegisterFailedFallback: 'Échec de l’enregistrement du raccourci global configuré.',
    openSettings: 'Paramètres',
    dismiss: 'Fermer',
  },
};

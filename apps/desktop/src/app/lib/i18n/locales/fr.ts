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
    fields: {
      id: 'id',
      sensitivity: 'sensibilité',
      source: 'source',
      size: 'taille',
      rank: 'rang',
    },
    none: '—',
    image: {
      loading: 'Chargement de l’image…',
      unavailable: 'Image indisponible.',
    },
  },
  status: {
    captureOn: 'Capture active',
    capturePaused: 'Capture en pause',
    aiOn: 'IA activée',
    aiOff: 'IA désactivée',
    entryCount: (n: number): string =>
      n <= 1 ? `${n} élément` : `${n.toLocaleString('fr')} éléments`,
    selectedCount: (n: number): string =>
      n <= 1 ? `${n} sélectionné` : `${n.toLocaleString('fr')} sélectionnés`,
  },
  actionMenu: {
    title: 'Actions IA',
    actions: {
      Summarize: 'Résumer',
      Translate: 'Traduire',
      FormatJson: 'Formater JSON',
      FormatMarkdown: 'Formater Markdown',
      ExplainCode: 'Expliquer le code',
      Rewrite: 'Réécrire',
      ExtractTasks: 'Extraire les tâches',
      RedactSecrets: 'Masquer les secrets',
    },
    tauriRequired: 'Les actions IA nécessitent le runtime Tauri.',
    resultTitle: 'Résultat',
    copyResult: 'Copier',
    copied: 'Copié',
    saveResult: 'Enregistrer comme nouvelle entrée',
    saved: 'Enregistré',
    closeResult: 'Fermer',
    runFailed: 'Échec de l’action IA.',
  },
  onboarding: {
    title: 'Terminer la configuration de Nagori',
    description: 'Certaines fonctionnalités nécessitent des autorisations macOS supplémentaires.',
    accessibilityRequired: 'Autorisation d’accessibilité requise',
    accessibilityHint:
      'Accordez l’accès à Accessibilité dans Réglages Système → Confidentialité et sécurité pour que Nagori puisse coller dans l’application active.',
    autoPasteDisabled:
      'Le collage automatique est désactivé — Entrée se contente de copier dans le presse-papiers tant que l’Accessibilité n’est pas accordée.',
    notificationsHint:
      'Autorisez les notifications pour recevoir les erreurs IA et les alertes de capture en pause.',
    openSettings: 'Ouvrir les Réglages Système',
    dismiss: 'Continuer sans',
  },
  settings: {
    title: 'Paramètres',
    backToPalette: 'Retour à la palette',
    loading: 'Chargement…',
    saving: 'Enregistrement…',
    save: 'Enregistrer',
    tauriRequired: 'L’enregistrement des paramètres nécessite le runtime Tauri.',
    tabs: {
      general: 'Général',
      privacy: 'Confidentialité',
      ai: 'IA',
      cli: 'CLI',
      advanced: 'Avancé',
    },
    capture: {
      legend: 'Capture',
      enabled: 'Enregistrer l’historique du presse-papiers',
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
      localOnly: 'Mode local uniquement (bloquer les appels IA distants)',
      appDenylist: 'Liste de refus d’apps',
      appDenylistHelp:
        'Un nom d’application source par ligne. Les captures depuis ces apps sont ignorées.',
      regexDenylist: 'Liste de refus regex',
      regexDenylistHelp:
        'Un regex Rust par ligne. Les captures correspondant à un motif sont ignorées.',
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
    },
    ai: {
      legend: 'IA',
      enabled: 'Activer les actions IA',
      provider: 'Fournisseur',
      providers: {
        none: 'Aucun',
        local: 'Local',
        remote: 'Distant',
      },
      semanticSearch: 'Activer la recherche sémantique',
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
      recentOrder: 'Ordre quand la recherche est vide',
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
      placeholder: 'ex. Cmd+Shift+P',
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
      autoCheckHelp:
        'Interroge le flux de versions régulièrement et affiche une bannière quand une nouvelle build est disponible. Le téléchargement n’est jamais installé sans votre confirmation.',
      autoCheckLocalOnly:
        'Désactivé tant que le mode local uniquement est actif. Désactivez-le (Confidentialité → Mode local uniquement) pour autoriser les vérifications.',
      channel: 'Canal',
      checkNow: 'Vérifier maintenant',
      checking: 'Vérification…',
      upToDate: 'Vous utilisez la dernière version.',
      available: 'Mise à jour disponible : {version}',
      viewRelease: 'Voir la version',
    },
  },
  keybindings: {
    selectNext: 'Résultat suivant',
    selectPrev: 'Résultat précédent',
    selectFirst: 'Aller au premier',
    selectLast: 'Aller au dernier',
    confirm: 'Coller la sélection',
    openActions: 'Ouvrir les actions IA',
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
    ai: 'Erreur du fournisseur IA.',
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
    openSettings: 'Paramètres',
    dismiss: 'Fermer',
  },
};

import type { Messages } from './en';

export const es: Messages = {
  palette: {
    placeholder: 'Buscar en el historial…',
    searching: 'Buscando…',
    resultCount: (count: number): string =>
      count === 1 ? '1 resultado' : `${count.toLocaleString('es')} resultados`,
    elapsed: (ms: number): string => `${ms.toFixed(0)} ms`,
    empty: 'Aún no hay historial.',
    fallback:
      '(Runtime de Tauri no iniciado) Los elementos copiados recientemente aparecerán aquí.',
    hints: {
      navigate: 'Navegar',
      paste: 'Pegar',
      actions: 'Acciones',
      settings: 'Ajustes',
    },
    filters: {
      toolbarLabel: 'Filtros rápidos',
      today: 'Hoy',
      last7days: 'Últimos 7 días',
      pinned: 'Fijados',
    },
  },
  preview: {
    empty: 'Selecciona un elemento para previsualizar.',
    loading: 'Cargando vista previa…',
    truncated: 'Vista previa recortada.',
    fields: {
      id: 'ID',
      sensitivity: 'sensibilidad',
      source: 'origen',
      size: 'tamaño',
      rank: 'rango',
      formats: 'formatos conservados',
    },
    none: '—',
    image: {
      loading: 'Cargando imagen…',
      unavailable: 'Imagen no disponible.',
    },
  },
  status: {
    captureOn: 'Captura activa',
    capturePaused: 'Captura en pausa',
    aiOn: 'IA activada',
    aiOff: 'IA desactivada',
    entryCount: (n: number): string =>
      n === 1 ? '1 elemento' : `${n.toLocaleString('es')} elementos`,
    selectedCount: (n: number): string =>
      n === 1 ? '1 seleccionado' : `${n.toLocaleString('es')} seleccionados`,
  },
  actionMenu: {
    title: 'Acciones de IA',
    actions: {
      Summarize: 'Resumir',
      Translate: 'Traducir',
      FormatJson: 'Formatear JSON',
      FormatMarkdown: 'Formatear Markdown',
      ExplainCode: 'Explicar el código',
      Rewrite: 'Reescribir',
      ExtractTasks: 'Extraer tareas',
      RedactSecrets: 'Ocultar secretos',
    },
    tauriRequired: 'Las acciones de IA requieren el runtime de Tauri.',
    resultTitle: 'Resultado',
    copyResult: 'Copiar',
    copied: 'Copiado',
    saveResult: 'Guardar como nueva entrada',
    saved: 'Guardado',
    closeResult: 'Cerrar',
    runFailed: 'La acción de IA falló.',
  },
  onboarding: {
    title: 'Termina la configuración de Nagori',
    description: 'Algunas funciones necesitan permisos adicionales de macOS para ejecutarse.',
    accessibilityRequired: 'Se requiere permiso de Accesibilidad',
    accessibilityHint:
      'Concede acceso a Accesibilidad en Ajustes del Sistema → Privacidad y seguridad para que Nagori pueda pegar en la app activa.',
    autoPasteDisabled:
      'El pegado automático está DESACTIVADO — Intro solo copia al portapapeles hasta que concedas Accesibilidad.',
    notificationsHint:
      'Permite las notificaciones para recibir alertas de errores de IA y de captura en pausa.',
    openSettings: 'Abrir Ajustes del Sistema',
    dismiss: 'Continuar sin ello',
  },
  settings: {
    title: 'Ajustes',
    backToPalette: 'Volver a la paleta',
    loading: 'Cargando…',
    saving: 'Guardando…',
    save: 'Guardar',
    tauriRequired: 'Guardar los ajustes requiere el runtime de Tauri.',
    tabs: {
      general: 'General',
      privacy: 'Privacidad',
      ai: 'IA',
      cli: 'CLI',
      advanced: 'Avanzado',
    },
    capture: {
      legend: 'Captura',
      enabled: 'Guardar historial del portapapeles',
      autoPaste: 'Pegar automáticamente con Intro',
      pasteFormatDefault: 'Formato de pegado por defecto',
      pasteFormatOptions: {
        preserve: 'Conservar',
        plain_text: 'Texto sin formato',
      },
      hotkey: 'Atajo global',
      captureInitialClipboard: 'Capturar el portapapeles al iniciar',
      captureInitialClipboardHelp:
        'Si está activado, el contenido del portapapeles al iniciar se añade al historial. Desactívalo para ignorar lo que ya estaba en el portapapeles.',
    },
    retention: {
      legend: 'Retención',
      maxCount: 'Máximo de entradas',
      maxDays: 'Retención (días)',
      maxDaysPlaceholder: '0 = ilimitado',
      maxDaysHelp: 'Pon 0 para conservar las entradas para siempre.',
      maxTotalBytes: 'Límite total de almacenamiento',
      maxTotalBytesPlaceholder: '0 = ilimitado',
      maxTotalBytesHelp: 'Las entradas fijadas se protegen aunque superen el límite.',
      maxBytes: 'Bytes máx. por entrada',
      pasteDelayMs: 'Retardo de pegado (ms)',
    },
    privacy: {
      legend: 'Filtros',
      localOnly: 'Modo solo local (bloquear llamadas a IA remota)',
      appDenylist: 'Lista de apps denegadas',
      appDenylistHelp:
        'Un nombre de aplicación de origen por línea. Las capturas desde estas apps se descartan.',
      regexDenylist: 'Lista de regex denegados',
      regexDenylistHelp:
        'Un regex de Rust por línea. Las capturas que coincidan con algún patrón se descartan.',
      secretHandling: 'Tratamiento de secretos',
      secretHandlingHelp:
        'Qué hacer cuando un clip se clasifica como secreto (claves API, JWT, claves privadas…).',
      secretHandlingOptions: {
        block: 'Bloquear — no almacenar',
        store_redacted: 'Almacenar redactado (predeterminado)',
        store_full: 'Almacenar completo (la vista previa sigue redactada)',
      },
      captureKinds: 'Tipos de captura',
      captureKindsHelp:
        'Los tipos desactivados se descartan antes de la clasificación de secretos.',
      captureKindOptions: {
        text: 'Texto',
        url: 'URL',
        code: 'Código',
        image: 'Imagen',
        fileList: 'Archivos',
        richText: 'Texto enriquecido',
        unknown: 'Desconocido',
      },
      storeFullWarning:
        'Aviso: «Almacenar completo» mantiene claves API, JWT y claves privadas en texto plano dentro de la base de datos SQLite local. La base de datos no está cifrada en reposo, así que cualquiera con acceso de lectura a tu carpeta personal (copias de seguridad, clientes de sincronización, malware) podría recuperar los secretos. Usa «Almacenar redactado» a menos que entiendas el riesgo.',
      storeFullConfirm:
        '¿Almacenar los secretos en texto plano? La base de datos no está cifrada; los secretos en bruto serán recuperables desde el disco y desde cualquier copia de seguridad que incluya el directorio de datos.',
    },
    ai: {
      legend: 'IA',
      enabled: 'Activar acciones de IA',
      provider: 'Proveedor',
      providers: {
        none: 'Ninguno',
        local: 'Local',
        remote: 'Remoto',
      },
      semanticSearch: 'Activar búsqueda semántica',
    },
    cli: {
      legend: 'CLI',
      ipcEnabled: 'Permitir conexiones IPC desde la CLI',
    },
    appearance: {
      legend: 'Apariencia',
      locale: 'Idioma',
      theme: 'Tema',
      themeOptions: {
        system: 'Sistema',
        light: 'Claro',
        dark: 'Oscuro',
      },
      recentOrder: 'Orden con búsqueda vacía',
      recentOrderOptions: {
        by_recency: 'Más recientes',
        by_use_count: 'Más usados',
        pinned_first_then_recency: 'Fijados primero',
      },
    },
    integration: {
      legend: 'Integración con el SO',
      autoLaunch: 'Iniciar al iniciar sesión',
      autoLaunchHelp:
        'Inicia Nagori al iniciar sesión utilizando el mecanismo nativo del sistema (LaunchAgent en macOS, clave Run del registro en Windows, autoarranque XDG en Linux).',
      menuBar: 'Mostrar icono en la bandeja',
      menuBarHelp:
        'Muestra el icono de bandeja de Nagori (macOS: barra de menús, Windows: área de notificación, Linux: indicador de estado). Desactívalo para una experiencia totalmente en segundo plano.',
      clearOnQuit: 'Borrar historial no fijado al salir',
      clearOnQuitHelp:
        'Al salir de la aplicación se eliminan todas las entradas no fijadas. Las entradas fijadas se conservan.',
    },
    display: {
      legend: 'Visualización de la paleta',
      rowCount: 'Filas visibles',
      rowCountHelp: 'Número máximo de filas de resultados antes de desplazarse (3–20).',
      previewPane: 'Mostrar panel de vista previa',
      previewPaneHelp: 'Ocúltalo para mantener la paleta compacta; la lista ocupa todo el ancho.',
    },
    hotkeys: {
      legend: 'Atajos',
      paletteHeading: 'Atajos de la paleta',
      paletteHelp:
        'Sobrescribe los atajos dentro de la paleta. Deja vacío un campo para conservar el valor por defecto.',
      secondaryHeading: 'Atajos globales secundarios',
      secondaryHelp:
        'Atajos opcionales a nivel de sistema registrados junto al atajo principal de la paleta.',
      placeholder: 'p. ej. Cmd+Shift+P',
      paletteActions: {
        pin: 'Fijar / desfijar selección',
        delete: 'Eliminar selección',
        'paste-as-plain': 'Pegar como texto sin formato',
        'copy-without-paste': 'Copiar sin pegar',
        clear: 'Vaciar la consulta',
        'open-preview': 'Alternar vista previa ampliada',
      },
      secondaryActions: {
        'repaste-last': 'Volver a pegar la entrada más reciente',
        'clear-history': 'Borrar historial no fijado',
      },
    },
    updates: {
      legend: 'Actualizaciones',
      autoCheck: 'Buscar actualizaciones automáticamente',
      autoCheckHelp:
        'Consulta el canal de versiones periódicamente y muestra un aviso cuando hay una nueva. La descarga nunca se instala sin tu confirmación.',
      autoCheckLocalOnly:
        'Desactivado mientras el modo solo local está activo. Apágalo en Privacidad → Modo solo local para permitir las comprobaciones.',
      channel: 'Canal',
      checkNow: 'Buscar ahora',
      checking: 'Buscando…',
      upToDate: 'Estás usando la última versión.',
      available: 'Actualización disponible: {version}',
      viewRelease: 'Ver la versión',
    },
  },
  keybindings: {
    selectNext: 'Siguiente resultado',
    selectPrev: 'Resultado anterior',
    selectFirst: 'Saltar al primero',
    selectLast: 'Saltar al último',
    confirm: 'Pegar selección',
    openActions: 'Abrir acciones de IA',
    togglePin: 'Fijar / desfijar',
    delete: 'Eliminar',
    openSettings: 'Abrir ajustes',
    close: 'Cerrar',
  },
  time: {
    justNow: 'justo ahora',
    minutesAgo: (n: number): string => (n === 1 ? 'hace 1 min' : `hace ${n} min`),
    hoursAgo: (n: number): string => (n === 1 ? 'hace 1 h' : `hace ${n} h`),
    daysAgo: (n: number): string => (n === 1 ? 'hace 1 día' : `hace ${n} días`),
  },
  errors: {
    unknown: 'Error desconocido.',
    storage: 'Error de almacenamiento.',
    search: 'Error de búsqueda.',
    platform: 'Error de plataforma.',
    permission: 'Permiso ausente.',
    ai: 'Error del proveedor de IA.',
    policy: 'Acción bloqueada por la política.',
    notFound: 'No encontrado.',
    invalidInput: 'Entrada no válida.',
    unsupported: 'No compatible con esta plataforma.',
    configuration: 'Error de configuración. Es un defecto de compilación — por favor, repórtalo.',
  },
  locales: {
    system: 'Sistema (seguir el SO)',
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
    autoPasteFailedTitle: 'Pegado automático fallido',
    autoPasteFailedFallback: 'El pegado automático falló.',
    openSettings: 'Ajustes',
    dismiss: 'Cerrar',
  },
};

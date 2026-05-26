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
    truncation: {
      headOnly: ({ shown, total }: { shown: string; total: string }): string =>
        `Se muestran los primeros ${shown} de ${total}.`,
      headAndTail: ({ elided }: { elided: string }): string =>
        `Se muestran el inicio y el final; se omiten ${elided} en el medio.`,
      elidedMatch: 'Una coincidencia de la búsqueda está en la zona omitida.',
      expand: 'Mostrar contenido completo',
      expanding: 'Cargando contenido completo…',
    },
    fields: {
      id: 'ID',
      sensitivity: 'sensibilidad',
      source: 'origen',
      size: 'tamaño',
      rank: 'rango',
      formats: 'formatos conservados',
    },
    none: '—',
    summary: {
      lines: (count: number): string =>
        count === 1 ? '1 línea' : `${count.toLocaleString('es')} líneas`,
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
      loading: 'Cargando imagen…',
      unavailable: 'Imagen no disponible.',
      alt: 'Vista previa de imagen del portapapeles',
    },
    fileList: {
      summary: (shown: number, total: number): string =>
        total === shown
          ? total === 1
            ? '1 archivo'
            : `${total.toLocaleString('es')} archivos`
          : `${shown.toLocaleString('es')} / ${total.toLocaleString('es')} archivos`,
      moreFiles: (count: number): string =>
        count === 1 ? '+1 archivo más' : `+${count.toLocaleString('es')} archivos más`,
      inFolder: (prefix: string): string => `en ${prefix}`,
    },
    url: {
      punycodeBadge: 'punycode',
      punycodeBadgeTitle: ({ ascii }: { ascii: string }): string =>
        `Host IDN. Forma ASCII: ${ascii}`,
      openHint: 'Pulsa Enter para abrir',
      confirmTitle: '¿Abrir este enlace?',
      confirmDescription: ({ host }: { host: string }): string =>
        `Nagori abrirá ${host} en tu navegador predeterminado.`,
      confirm: 'Abrir',
      cancel: 'Cancelar',
      openFailed: 'No se pudo abrir el URL.',
    },
  },
  status: {
    captureOn: 'Captura activa',
    capturePaused: 'Captura en pausa',
    entryCount: (n: number): string =>
      n === 1 ? '1 elemento' : `${n.toLocaleString('es')} elementos`,
    selectedCount: (n: number): string =>
      n === 1 ? '1 seleccionado' : `${n.toLocaleString('es')} seleccionados`,
    autoPasteOff: 'Pegado automático desactivado — Accesibilidad no autorizada',
    autoPasteOffShort: '⚠ Pegado automático desactivado',
    autoPasteOffSetupAria:
      'Pegado automático desactivado: se requiere permiso de Accesibilidad. Abrir configuración.',
  },
  actionMenu: {
    title: 'Acciones rápidas',
    actions: {
      Summarize: 'Resumir',
      FormatJson: 'Formatear JSON',
      ExtractTasks: 'Extraer tareas',
      RedactSecrets: 'Ocultar secretos',
    },
    tauriRequired: 'Las acciones rápidas requieren el runtime de Tauri.',
    resultTitle: 'Resultado',
    copyResult: 'Copiar',
    copied: 'Copiado',
    saveResult: 'Guardar como nueva entrada',
    saved: 'Guardado',
    closeResult: 'Cerrar',
    runFailed: 'La acción rápida falló.',
  },
  setup: {
    title: 'Configurar Nagori',
    intro:
      'Concede los permisos que Nagori necesita para pegar en otras apps. Puedes cambiarlos más adelante en Ajustes del Sistema.',
    accessibility: {
      title: 'Accesibilidad',
      required: 'Obligatorio',
      description:
        'Habilitar Accesibilidad permite que Nagori pegue entradas del historial directamente en la app activa. Pulsa «Conceder Accesibilidad» para abrir el diálogo de macOS y activa el interruptor de Nagori.',
      descriptionLinux:
        'Instala el paquete `wtype` en una sesión Wayland para que Nagori pueda sintetizar Ctrl+V en la app activa.',
      screenshotAlt:
        'Ajustes del Sistema → Privacidad y seguridad → Accesibilidad con el interruptor de Nagori resaltado.',
      grantButton: 'Conceder Accesibilidad…',
      grantButtonRetry: 'Abrir Ajustes del Sistema',
      recheckButton: 'Volver a comprobar',
      requesting: 'Solicitando…',
      states: {
        NotRequested: 'No solicitado',
        PromptShownNotGranted: 'Requiere acción',
        Granted: 'Concedido',
        RevokedAfterGranted: 'Reactivar',
        Unavailable: 'No aplicable',
      },
      statusLabel: 'Estado',
      messages: {
        NotRequested:
          'Nagori aún no ha pedido permiso de Accesibilidad a macOS. Pulsa el botón para mostrar el diálogo del sistema.',
        PromptShownNotGranted:
          'macOS no mostrará el diálogo una segunda vez. Abre Ajustes del Sistema y activa Nagori en la lista de Accesibilidad.',
        Granted: 'El pegado automático está listo.',
        RevokedAfterGranted:
          'A Nagori se le concedió Accesibilidad anteriormente. Reactívala en Ajustes del Sistema para recuperar el pegado automático.',
        UnavailableMacosFallback:
          'El estado de Accesibilidad no está disponible en esta compilación.',
        UnavailableWindows:
          'Windows no requiere un permiso equivalente a Accesibilidad para el pegado automático.',
        UnavailableLinux:
          'El pegado automático en Linux depende del asistente `wtype`. Instálalo mediante tu gestor de paquetes.',
      },
      timeoutError:
        'No se detectó la concesión en 60 s. Abre Ajustes del Sistema → Privacidad y seguridad → Accesibilidad, comprueba el interruptor de Nagori y pulsa «Volver a comprobar».',
      requestError:
        'No se pudo iniciar la solicitud de Accesibilidad — consulta Console.app para más detalles.',
    },
  },
  settings: {
    title: 'Ajustes',
    backToPalette: 'Volver a la paleta',
    loading: 'Cargando…',
    statusSaving: 'Guardando…',
    statusSaved: 'Guardado',
    statusError: 'Error al guardar: {error}',
    tauriRequired: 'Guardar los ajustes requiere el runtime de Tauri.',
    tabs: {
      setup: 'Configuración',
      general: 'General',
      privacy: 'Privacidad',
      cli: 'CLI',
      advanced: 'Avanzado',
    },
    capture: {
      legend: 'Captura',
      enabled: 'Activar la captura del portapapeles',
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
      appDenylist: 'Lista de apps denegadas',
      appDenylistHelp:
        'Un nombre de aplicación de origen por línea. Las capturas desde estas apps se descartan.',
      regexDenylist: 'Lista de regex denegados',
      regexDenylistHelp:
        'Un patrón por línea (p. ej. INTERNAL-\\d+). Las coincidencias se descartan antes de llegar al historial. Cada patrón debe tener menos de 256 bytes (UTF-8) y un máximo de 3 niveles de paréntesis ( ) sin escapar; divide las reglas complejas en varias líneas en lugar de anidar grupos.',
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
      regexDenylistAutosaveHint:
        'Los cambios se guardan automáticamente cuando se corrigen los errores de regex resaltados.',
      regexErrors: {
        lineLabel: 'Línea {line}:',
        tooLong:
          'demasiado largo ({bytes} bytes > {limit}). Divide el patrón en varias líneas o elimina alternativas sin usar.',
        tooNested:
          'anidamiento de paréntesis {depth} supera el límite de {limit}. Aplana los grupos (usa (?: … ) una sola vez) o divide en varias líneas.',
        invalidSyntax:
          'sintaxis de regex inválida: {error}. Escapa los metacaracteres literales con \\\\ o reescribe el patrón.',
        empty: 'entrada vacía: elimina la línea en blanco o escribe un patrón.',
      },
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
      recentOrder: 'Orden del historial',
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
      placeholder: 'Haz clic para grabar',
      recordingHint: 'Pulsa el atajo… (Esc para cancelar)',
      clearAriaLabel: 'Borrar atajo',
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
      channel: 'Canal',
      checkNow: 'Buscar',
      checking: 'Buscando…',
      upToDate: 'Estás usando la última versión.',
      available: 'Actualización disponible: {version}',
      availableManual:
        'Actualización disponible: {version}. Tu modo de instalación no admite la actualización en sitio: descarga la nueva compilación desde GitHub.',
      viewRelease: 'Ver la versión',
      downloadManual: 'Descargar desde GitHub',
    },
    capabilities: {
      legend: 'Capacidades de la plataforma',
      help: 'Lo que Nagori puede usar en tu sistema operativo actual. Las funciones marcadas como «Permiso necesario» quedan disponibles tras conceder acceso en los ajustes del sistema operativo.',
      platform: 'Plataforma',
      tier: 'Nivel',
      openSetup: 'Abrir configuración',
      columns: { capability: 'Capacidad', status: 'Estado', detail: 'Detalle' },
      statuses: {
        available: 'Disponible',
        unsupported: 'No compatible',
        requiresPermission: 'Permiso necesario',
        requiresExternalTool: 'Herramienta externa',
        experimental: 'Experimental',
      },
      rows: {
        captureText: 'Capturar texto',
        captureImage: 'Capturar imagen',
        captureFiles: 'Capturar archivos',
        writeText: 'Escribir texto',
        writeImage: 'Escribir imagen',
        clipboardMultiRepresentationWrite: 'Reescritura multi-representación',
        autoPaste: 'Pegado automático',
        globalHotkey: 'Atajo global',
        frontmostApp: 'Aplicación en primer plano',
        permissionsUi: 'IU de permisos',
        updateCheck: 'Comprobación de actualizaciones',
        previewQuickLook: 'Vista previa (Quick Look)',
      },
    },
  },
  keybindings: {
    selectNext: 'Siguiente resultado',
    selectPrev: 'Resultado anterior',
    selectFirst: 'Saltar al primero',
    selectLast: 'Saltar al último',
    confirm: 'Pegar selección',
    openActions: 'Abrir acciones rápidas',
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
    ai: 'Error de acción rápida.',
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
    hotkeyRegisterFailedTitle: 'Atajo no disponible',
    hotkeyRegisterFailedFallback: 'No se pudo registrar el atajo global configurado.',
    openSettings: 'Ajustes',
    dismiss: 'Cerrar',
    accessibilityGrantedTitle: 'Accesibilidad concedida',
  },
};

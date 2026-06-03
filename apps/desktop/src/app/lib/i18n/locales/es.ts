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
    screenshotBadge: 'Captura',
    hints: {
      navigate: 'Navegar',
      paste: 'Pegar',
      pin: 'Fijar',
      actions: 'Acciones',
      settings: 'Ajustes',
    },
    filters: {
      toolbarLabel: 'Filtros rápidos',
      today: 'Hoy',
      yesterday: 'Ayer',
      last7days: 'Últimos 7 días',
      last30days: 'Últimos 30 días',
      pinned: 'Fijados',
      kindText: 'Texto',
      kindUrl: 'URL',
      kindCode: 'Código',
      kindImage: 'Imagen',
      kindFiles: 'Archivos',
      dateGroup: 'Fecha',
      typeGroup: 'Tipo',
      sourceGroup: 'App de origen',
      sourceShort: 'App',
      allApps: 'Todas las apps',
      clear: 'Borrar filtros',
    },
  },
  rankReason: {
    exact: 'Exacto',
    prefix: 'Prefijo',
    substring: 'Coincidencia',
    fullText: 'Texto',
    fuzzy: 'Aprox.',
    semantic: 'Semántico',
    recent: 'Reciente',
    frequent: 'Frecuente',
    pinned: 'Fijado',
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
    pasteDiagnostics: {
      label: '⚠ Falló el pegado automático',
      toolFallback: 'la herramienta de pegado',
      hint: {
        accessibilityMissing:
          'Falló el pegado automático: se requiere permiso de Accesibilidad. Copiado — pega manualmente.',
        toolMissing: ({ tool }) =>
          `Falló el pegado automático: ${tool} no está instalado. Copiado — instala ${tool} o pega manualmente.`,
        timeout:
          'El pegado automático agotó el tiempo de espera; el compositor puede estar ocupado. Copiado — pega manualmente o reintenta.',
        synthUnsupported:
          'El pegado automático no está disponible en esta plataforma. Copiado — pega manualmente.',
        previousAppLost:
          'Pegado automático omitido: no se pudo volver a la app de origen. Copiado — pega manualmente.',
        unknown: 'Falló el pegado automático. Copiado — pega manualmente.',
      },
    },
  },
  actionMenu: {
    title: 'Acciones rápidas',
    close: 'Cerrar',
    actions: {
      SummarizeFirstSentence: 'Resumir (primera frase)',
      FormatJson: 'Formatear JSON',
      ExtractTasks: 'Extraer tareas',
      RedactSecrets: 'Ocultar secretos',
    },
    aiActions: {
      Summarize: 'Resumir',
      Rewrite: 'Reescribir',
      FormatMarkdown: 'Formatear como Markdown',
      ExtractTasks: 'Organizar tareas',
      ExplainCode: 'Explicar código',
    },
    aiBadge: 'IA',
    aiCancel: 'Cancelar',
    aiUnavailable: 'Las acciones de IA no están disponibles en este momento.',
    aiRemediation: {
      'ai.unavailable.apple_intelligence_not_enabled':
        'Activa Apple Intelligence en Ajustes del sistema para usar acciones de IA.',
      'ai.unavailable.device_not_eligible':
        'Este Mac no es compatible con Apple Intelligence (se requiere Apple silicon).',
      'ai.unavailable.model_not_ready':
        'El modelo en el dispositivo aún se está descargando. Inténtalo de nuevo en breve.',
      'ai.unavailable.asset_missing': 'Un recurso necesario en el dispositivo no está disponible.',
      'ai.unavailable.rate_limited':
        'El modelo en el dispositivo está ocupado. Inténtalo de nuevo en breve.',
    },
    tauriRequired: 'Las acciones rápidas requieren el runtime de Tauri.',
    generating: 'Generando…',
    working: 'Procesando…',
    done: 'Listo',
    resultTitle: 'Resultado',
    copyResult: 'Copiar',
    copied: 'Copiado',
    saveResult: 'Guardar como nueva entrada',
    saved: 'Guardado',
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
      descriptionWindows:
        'En Windows, Nagori pega en la app activa sin ningún permiso equivalente a Accesibilidad: aquí no hay nada que configurar.',
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
      ai: 'IA',
      cli: 'CLI',
      advanced: 'Avanzado',
    },
    ai: {
      legend: 'IA',
      enabled: 'Habilitar acciones de IA',
      enabledHelp:
        'Las acciones con modelo como Resumir se ejecutan totalmente en el dispositivo mediante Apple Intelligence. Desactivado por defecto.',
      provider: 'Proveedor',
      providerDisabled: 'Desactivado',
      providerApple: 'Apple (en el dispositivo)',
      allowStreaming: 'Transmitir los resultados a medida que se generan',
      allowStreamingHelp:
        'Muestra la salida parcial mientras el modelo escribe. Desactívalo para ver solo el resultado final.',
      semanticIndex: 'Índice de búsqueda semántica',
      semanticIndexHelp:
        'Crea embeddings en el dispositivo para que la búsqueda coincida por significado, no solo por texto. Usa un modelo de embeddings de Apple en el dispositivo (macOS); desactivado por defecto.',
      semanticIndexAcPowerOnly: 'Indexar solo con corriente alterna',
      semanticIndexAcPowerOnlyHelp:
        'Pausa el embedding en segundo plano con batería para ahorrar energía. Desactívalo para indexar también con batería.',
      semanticIndexRebuild: 'Reconstruir índice',
      semanticIndexStatus: 'Estado del índice',
      semanticIndexStateReady: 'Actualizado',
      semanticIndexStateIndexing: 'Indexando…',
      semanticIndexStatePaused: 'En pausa (con batería)',
      semanticIndexStateUnavailable: 'Modelo de embedding no disponible',
      semanticIndexStateUnsupported: 'No compatible con este dispositivo',
      semanticIndexStateDisabled: 'Desactivado',
      status: 'Disponibilidad',
      statusAvailable: 'Disponible',
      statusUnavailable: 'No disponible',
      statusDisabled: 'Desactivado',
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
      appDenylistPasswordManagers: 'Bloquear gestores de contraseñas',
      appDenylistPasswordManagersHelp:
        'Descarta las capturas de los gestores de contraseñas incluidos (1Password, Bitwarden, KeePassXC, Apple Passwords) usando identificadores exactos de app. El preset es fijo y no editable. Recomendado; déjalo activado salvo que necesites pegar activamente desde un gestor de contraseñas vía el portapapeles.',
      appDenylistPatterns: 'Patrones personalizados',
      appDenylistPatternsHelp:
        'Una subcadena por línea: se descartan las capturas cuyo nombre de app de origen, bundle ID o ruta de ejecutable contengan alguna (sin distinguir mayúsculas/minúsculas). Usa esta lista para apps fuera del preset, por ejemplo Dashlane, LastPass o herramientas internas.',
      appDenylistUnsupported:
        'Tu sesión de escritorio no expone la app en primer plano, por lo que el bloqueo por app no coincidiría con nada. Usa la lista de regex denegados o «Tipos de captura» de abajo para limitar lo que se captura.',
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
      install: {
        legend: 'Herramienta de línea de comandos',
        help: 'Instala la herramienta de línea de comandos `nagori` incluida en ~/.local/bin para buscar y pegar el historial desde una terminal.',
        button: 'Instalar la CLI de nagori',
        reinstall: 'Reinstalar',
        installing: 'Instalando…',
        statusInstalled: 'nagori está enlazado en {path}.',
        statusNotInstalled: 'La herramienta de línea de comandos nagori aún no está instalada.',
        installed: 'nagori se instaló en {path}.',
        installedNeedsPath:
          'nagori se instaló en {path}. Añade el directorio de abajo a tu PATH para usarlo.',
        notOnPath:
          '{dir} aún no está en tu PATH. Añade esta línea a tu perfil de shell (p. ej. ~/.zshrc) y abre una terminal nueva:',
        pathExport: 'export PATH="$HOME/.local/bin:$PATH"',
        unavailable:
          'La CLI incluida solo viene con la app empaquetada, no con las compilaciones de desarrollo.',
        unsupported:
          'La instalación con un clic no está disponible en esta plataforma. Copia el ejecutable nagori incluido a un directorio de tu PATH.',
      },
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
    internal: 'Algo salió mal. Inténtalo de nuevo.',
    forbidden: 'No disponible para esta entrada.',
    paste: 'Falló el pegado automático. Copiado — pega manualmente.',
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

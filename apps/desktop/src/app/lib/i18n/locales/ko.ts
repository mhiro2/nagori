import type { Messages } from './en';

export const ko: Messages = {
  palette: {
    placeholder: '기록 검색…',
    searching: '검색 중…',
    resultCount: (count: number): string => `${count.toLocaleString('ko')}건`,
    elapsed: (ms: number): string => `${ms.toFixed(0)} ms`,
    empty: '기록이 아직 없습니다.',
    fallback: '(Tauri 런타임이 시작되지 않음) 최근에 복사한 항목이 여기에 표시됩니다.',
    hints: {
      navigate: '이동',
      paste: '붙여넣기',
      pin: '고정',
      actions: '동작',
      settings: '설정',
    },
    filters: {
      toolbarLabel: '빠른 필터',
      today: '오늘',
      yesterday: '어제',
      last7days: '최근 7일',
      last30days: '최근 30일',
      pinned: '고정됨',
      kindText: '텍스트',
      kindUrl: 'URL',
      kindCode: '코드',
      kindImage: '이미지',
      kindFiles: '파일',
      dateGroup: '날짜',
      typeGroup: '유형',
      sourceGroup: '출처 앱',
      sourceShort: '앱',
      allApps: '모든 앱',
      clear: '필터 지우기',
    },
  },
  rankReason: {
    exact: '정확',
    prefix: '접두',
    substring: '부분',
    fullText: '전문',
    fuzzy: '유사',
    semantic: '의미',
    recent: '최근',
    frequent: '자주',
    pinned: '고정',
  },
  preview: {
    empty: '미리 볼 항목을 선택하세요.',
    loading: '미리보기 불러오는 중…',
    truncated: '미리보기가 잘렸습니다.',
    truncation: {
      headOnly: ({ shown, total }: { shown: string; total: string }): string =>
        `전체 ${total} 중 앞부분 ${shown}를 표시합니다.`,
      headAndTail: ({ elided }: { elided: string }): string =>
        `앞부분과 끝부분을 표시합니다. 중간 ${elided}는 생략했습니다.`,
      elidedMatch: '생략된 중간 영역에 검색 일치 항목이 있습니다.',
      expand: '전체 내용 보기',
      expanding: '전체 내용 불러오는 중…',
    },
    fields: {
      id: 'ID',
      sensitivity: '민감도',
      source: '출처',
      size: '크기',
      rank: '순위',
      formats: '보존된 형식',
    },
    none: '—',
    summary: {
      lines: (count: number): string => `${count.toLocaleString('ko')}줄`,
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
      loading: '이미지 불러오는 중…',
      unavailable: '이미지를 표시할 수 없습니다.',
      alt: '클립보드 이미지 미리보기',
    },
    fileList: {
      summary: (shown: number, total: number): string =>
        total === shown
          ? `${total.toLocaleString('ko')}개`
          : `${shown.toLocaleString('ko')} / ${total.toLocaleString('ko')}개`,
      moreFiles: (count: number): string => `+${count.toLocaleString('ko')}개 더`,
      inFolder: (prefix: string): string => `${prefix} 하위`,
    },
    url: {
      punycodeBadge: 'punycode',
      punycodeBadgeTitle: ({ ascii }: { ascii: string }): string =>
        `IDN 호스트입니다. ASCII 표기: ${ascii}`,
      openHint: 'Enter로 열기',
      confirmTitle: '이 링크를 열까요?',
      confirmDescription: ({ host }: { host: string }): string =>
        `기본 브라우저에서 ${host}을(를) 엽니다.`,
      confirm: '열기',
      cancel: '취소',
      openFailed: 'URL을 열 수 없습니다.',
    },
  },
  status: {
    captureOn: '캡처 켜짐',
    capturePaused: '캡처 일시 중지',
    entryCount: (n: number): string => `${n.toLocaleString('ko')}개`,
    selectedCount: (n: number): string => `${n.toLocaleString('ko')}개 선택됨`,
    autoPasteOff: '자동 붙여넣기 꺼짐 — Accessibility 권한 없음',
    autoPasteOffShort: '⚠ 자동 붙여넣기 꺼짐',
    autoPasteOffSetupAria: '자동 붙여넣기 꺼짐: Accessibility 권한이 필요합니다. 설정 열기.',
  },
  actionMenu: {
    title: '빠른 동작',
    close: '닫기',
    actions: {
      SummarizeFirstSentence: '요약(첫 문장)',
      FormatJson: 'JSON 정리',
      ExtractTasks: '작업 추출',
      RedactSecrets: '비밀 정보 마스킹',
    },
    aiActions: {
      Summarize: '요약',
      Rewrite: '다시 쓰기',
      FormatMarkdown: 'Markdown 형식화',
      ExtractTasks: '작업 정리',
      ExplainCode: '코드 설명',
    },
    aiBadge: 'AI',
    aiCancel: '취소',
    aiUnavailable: '지금은 AI 작업을 사용할 수 없습니다.',
    aiRemediation: {
      'ai.unavailable.apple_intelligence_not_enabled':
        'AI 동작을 사용하려면 시스템 설정에서 Apple Intelligence를 켜세요.',
      'ai.unavailable.device_not_eligible':
        '이 Mac은 Apple Intelligence를 지원하지 않습니다(Apple 실리콘 필요).',
      'ai.unavailable.model_not_ready':
        '온디바이스 모델을 다운로드 중입니다. 잠시 후 다시 시도하세요.',
      'ai.unavailable.asset_missing': '필요한 온디바이스 자산을 사용할 수 없습니다.',
      'ai.unavailable.rate_limited': '온디바이스 모델이 사용 중입니다. 잠시 후 다시 시도하세요.',
    },
    tauriRequired: '빠른 동작에는 Tauri 런타임이 필요합니다.',
    generating: '생성 중…',
    working: '처리 중…',
    done: '완료',
    resultTitle: '결과',
    copyResult: '복사',
    copied: '복사됨',
    saveResult: '새 항목으로 저장',
    saved: '저장됨',
  },
  setup: {
    title: 'Nagori 설정',
    intro:
      'Nagori가 다른 앱에 붙여넣기 위해 필요한 권한을 허용하세요. 시스템 설정에서 나중에 변경할 수 있습니다.',
    accessibility: {
      title: '접근성',
      required: '필수',
      description:
        '접근성을 허용하면 Nagori가 기록을 포커스된 앱에 직접 붙여넣을 수 있습니다. “접근성 허용…”을 눌러 macOS 대화상자를 열고 Nagori 스위치를 켜세요.',
      descriptionLinux:
        'Wayland 세션에서 `wtype` 패키지를 설치하면 Nagori가 포커스된 앱에 Ctrl+V를 합성할 수 있습니다.',
      descriptionWindows:
        'Windows에서는 접근성에 해당하는 권한 없이도 Nagori가 포커스된 앱에 붙여넣을 수 있습니다. 여기서 설정할 항목은 없습니다.',
      screenshotAlt:
        '시스템 설정 → 개인 정보 보호 및 보안 → 접근성에서 Nagori 토글을 강조 표시한 스크린샷.',
      grantButton: '접근성 허용…',
      grantButtonRetry: '시스템 설정 열기',
      recheckButton: '다시 확인',
      requesting: '요청 중…',
      states: {
        NotRequested: '요청 안 됨',
        PromptShownNotGranted: '조치 필요',
        Granted: '허용됨',
        RevokedAfterGranted: '다시 활성화',
        Unavailable: '해당 없음',
      },
      statusLabel: '상태',
      messages: {
        NotRequested:
          'Nagori는 아직 macOS에 접근성을 요청하지 않았습니다. 아래 버튼을 누르면 시스템 대화상자가 표시됩니다.',
        PromptShownNotGranted:
          'macOS는 대화상자를 두 번 표시하지 않습니다. 시스템 설정을 열고 접근성 목록에서 Nagori를 켜세요.',
        Granted: '자동 붙여넣기를 사용할 수 있습니다.',
        RevokedAfterGranted:
          'Nagori는 이전에 접근성 권한을 받았습니다. 시스템 설정에서 다시 활성화하면 자동 붙여넣기를 복원할 수 있습니다.',
        UnavailableMacosFallback: '이 빌드에서는 접근성 상태를 사용할 수 없습니다.',
        UnavailableWindows:
          'Windows에서는 자동 붙여넣기에 접근성에 해당하는 권한이 필요하지 않습니다.',
        UnavailableLinux:
          'Linux의 자동 붙여넣기는 `wtype` 도우미에 의존합니다. 패키지 관리자를 통해 설치하세요.',
      },
      timeoutError:
        '60초 이내에 권한 허용을 감지하지 못했습니다. 시스템 설정 → 개인 정보 보호 및 보안 → 접근성에서 Nagori 스위치를 확인한 뒤 “다시 확인”을 누르세요.',
      requestError: '접근성 요청을 시작할 수 없습니다. 자세한 내용은 Console.app을 확인하세요.',
    },
  },
  settings: {
    title: '설정',
    backToPalette: '팔레트로 돌아가기',
    loading: '불러오는 중…',
    statusSaving: '저장 중…',
    statusSaved: '저장됨',
    statusError: '저장 실패: {error}',
    tauriRequired: '설정 저장에는 Tauri 런타임이 필요합니다.',
    tabs: {
      setup: '설정 시작',
      general: '일반',
      privacy: '개인 정보',
      ai: 'AI',
      cli: 'CLI',
      advanced: '고급',
    },
    ai: {
      legend: 'AI',
      enabled: 'AI 동작 사용',
      enabledHelp:
        '요약 같은 모델 기반 동작은 Apple Intelligence를 통해 기기에서 실행됩니다. 기본값은 꺼짐입니다.',
      provider: '공급자',
      providerDisabled: '사용 안 함',
      providerApple: 'Apple(온디바이스)',
      allowStreaming: '생성되는 대로 결과 스트리밍',
      allowStreamingHelp:
        '모델이 작성하는 동안 부분 출력을 표시합니다. 끄면 최종 결과만 표시합니다.',
      semanticIndex: '시맨틱 검색 인덱스',
      semanticIndexHelp:
        '온디바이스 임베딩을 만들어 텍스트뿐 아니라 의미로도 검색할 수 있게 합니다. Apple의 온디바이스 임베딩 모델(macOS)을 사용하며 기본값은 꺼짐입니다.',
      semanticIndexAcPowerOnly: 'AC 전원 연결 시에만 인덱싱',
      semanticIndexAcPowerOnlyHelp:
        '배터리 사용 중에는 백그라운드 임베딩을 일시 중지하여 전력을 아낍니다. 끄면 배터리에서도 인덱싱합니다.',
      semanticIndexRebuild: '인덱스 다시 만들기',
      semanticIndexStatus: '인덱스 상태',
      semanticIndexStateReady: '최신 상태',
      semanticIndexStateIndexing: '인덱싱 중…',
      semanticIndexStatePaused: '일시 중지됨(배터리 사용 중)',
      semanticIndexStateUnavailable: '임베딩 모델을 사용할 수 없음',
      semanticIndexStateUnsupported: '이 기기에서 지원되지 않음',
      semanticIndexStateDisabled: '사용 안 함',
      status: '사용 가능 여부',
      statusAvailable: '사용 가능',
      statusUnavailable: '사용 불가',
      statusDisabled: '사용 안 함',
    },
    capture: {
      legend: '캡처',
      enabled: '클립보드 캡처 사용',
      autoPaste: 'Enter에서 자동 붙여넣기',
      pasteFormatDefault: '기본 붙여넣기 형식',
      pasteFormatOptions: {
        preserve: '원본 형식',
        plain_text: '일반 텍스트',
      },
      hotkey: '전역 단축키',
      captureInitialClipboard: '시작 시 클립보드 캡처',
      captureInitialClipboardHelp:
        '활성화하면 시작 시점의 클립보드 내용을 기록에 추가합니다. 비활성화하면 기존 내용은 무시됩니다.',
    },
    retention: {
      legend: '보존',
      maxCount: '최대 항목 수',
      maxDays: '보존 기간(일)',
      maxDaysPlaceholder: '0 = 무제한',
      maxDaysHelp: '0으로 설정하면 항목을 무기한 보존합니다.',
      maxTotalBytes: '전체 저장 용량 제한',
      maxTotalBytesPlaceholder: '0 = 무제한',
      maxTotalBytesHelp: '고정된 항목은 제한을 초과해도 보호됩니다.',
      maxBytes: '항목당 최대 바이트',
      pasteDelayMs: '붙여넣기 지연(ms)',
    },
    privacy: {
      legend: '필터',
      appDenylistPasswordManagers: '비밀번호 관리자 차단',
      appDenylistPasswordManagersHelp:
        '번들 프리셋(1Password, Bitwarden, KeePassXC, Apple Passwords)에서의 복사를 정확한 앱 식별자로 차단합니다. 프리셋 내용은 고정되어 편집할 수 없습니다. 비밀번호 관리자에서 클립보드로 붙여넣을 일이 없다면 켜둔 채로 두는 것을 권장합니다.',
      appDenylistPatterns: '사용자 지정 패턴',
      appDenylistPatternsHelp:
        '한 줄에 부분 문자열 하나씩 입력합니다. 소스 앱 이름, 번들 ID, 실행 파일 경로 중 하나라도 포함되면 캡처되지 않습니다(대소문자 구분 없음). 프리셋에 없는 앱(Dashlane / LastPass / 사내 도구 등)을 차단하고 싶을 때 여기에 추가하세요.',
      appDenylistUnsupported:
        '현재 데스크톱 세션은 최상위 앱 정보를 제공하지 않으므로 앱별 차단이 동작하지 않습니다. 아래의 정규식 차단 목록이나 캡처 종류로 캡처 범위를 제한하세요.',
      regexDenylist: '정규식 차단 목록',
      regexDenylistHelp:
        '한 줄에 하나의 패턴(예: INTERNAL-\\d+). 일치하는 내용은 기록에 저장되지 않습니다. 각 패턴은 256바이트(UTF-8) 이하, 이스케이프하지 않은 ( ) 중첩은 최대 3단계까지 유지하세요. 복잡한 규칙은 그룹을 중첩하지 말고 여러 줄로 나눠 작성합니다.',
      secretHandling: '보안 정보 처리',
      secretHandlingHelp: 'API 키, JWT, 개인 키 등 비밀 정보로 분류된 항목을 저장할 때의 동작.',
      secretHandlingOptions: {
        block: '저장하지 않음(차단)',
        store_redacted: '마스킹된 상태로 저장(기본값)',
        store_full: '원문 그대로 저장(미리보기는 마스킹)',
      },
      captureKinds: '캡처 대상',
      captureKindsHelp: '꺼진 종류는 보안 정보 분류 전에 제외됩니다.',
      captureKindOptions: {
        text: '텍스트',
        url: 'URL',
        code: '코드',
        image: '이미지',
        fileList: '파일',
        richText: '서식 있는 텍스트',
        unknown: '알 수 없음',
      },
      storeFullWarning:
        "경고: '원문 그대로 저장'을 선택하면 API 키, JWT, 개인 키와 같은 비밀이 로컬 SQLite DB에 평문으로 남습니다. DB는 암호화되어 있지 않으므로 홈 디렉터리에 접근할 수 있는 모든 주체(백업, 동기화 클라이언트, 악성코드 등)가 비밀을 복원할 수 있습니다. 위험을 충분히 이해하지 못했다면 '마스킹된 상태로 저장'을 권장합니다.",
      storeFullConfirm:
        '비밀을 평문으로 저장하시겠습니까? DB는 암호화되지 않으며, 데이터 디렉터리 백업을 포함해 디스크에서 원문을 복원할 수 있습니다.',
      regexDenylistAutosaveHint: '강조 표시된 정규식 오류를 수정하면 자동으로 저장됩니다.',
      regexErrors: {
        lineLabel: '{line}번째 줄:',
        tooLong:
          '너무 깁니다({bytes}바이트 > {limit}). 여러 줄로 나누거나 사용하지 않는 분기점을 제거하세요.',
        tooNested:
          '괄호 중첩 깊이 {depth}가 한계 {limit}를 초과합니다. 캡처하지 않는 그룹(?: … )을 한 번만 사용하거나 여러 줄로 나누세요.',
        invalidSyntax:
          '정규식 구문 오류: {error}. 리터럴 메타 문자를 \\\\로 이스케이프하거나 패턴을 수정하세요.',
        empty: '빈 항목입니다. 빈 줄을 제거하거나 패턴을 입력하세요.',
      },
    },
    cli: {
      legend: 'CLI',
      ipcEnabled: 'CLI에서 IPC 연결 허용',
      install: {
        legend: '명령줄 도구',
        help: '번들된 `nagori` 명령줄 도구를 ~/.local/bin에 설치하면 터미널에서 기록을 검색하고 붙여넣을 수 있습니다.',
        button: 'nagori CLI 설치',
        reinstall: '다시 설치',
        installing: '설치 중…',
        statusInstalled: 'nagori가 {path}에 연결되어 있습니다.',
        statusNotInstalled: 'nagori 명령줄 도구가 아직 설치되지 않았습니다.',
        installed: 'nagori를 {path}에 설치했습니다.',
        installedNeedsPath:
          'nagori를 {path}에 설치했습니다. 사용하려면 아래 디렉터리를 PATH에 추가하세요.',
        notOnPath:
          '{dir}가 아직 PATH에 없습니다. 다음 줄을 셸 프로필(예: ~/.zshrc)에 추가한 뒤 새 터미널을 여세요:',
        pathExport: 'export PATH="$HOME/.local/bin:$PATH"',
        unavailable: '번들된 CLI는 패키지된 앱에만 포함되며 개발 빌드에는 포함되지 않습니다.',
        unsupported:
          '이 플랫폼에서는 원클릭 설치를 사용할 수 없습니다. 번들된 nagori 실행 파일을 PATH에 있는 디렉터리로 복사하세요.',
      },
    },
    appearance: {
      legend: '표시',
      locale: '언어',
      theme: '테마',
      themeOptions: {
        system: '시스템',
        light: '라이트',
        dark: '다크',
      },
      recentOrder: '기록 정렬',
      recentOrderOptions: {
        by_recency: '최근순',
        by_use_count: '사용 횟수순',
        pinned_first_then_recency: '고정 항목 우선',
      },
    },
    integration: {
      legend: 'OS 통합',
      autoLaunch: '로그인 시 자동 실행',
      autoLaunchHelp:
        '운영체제 기본 기능(macOS: LaunchAgent / Windows: Run 레지스트리 / Linux: XDG 자동 시작)을 통해 로그인 시 Nagori를 실행합니다.',
      menuBar: '트레이 아이콘 표시',
      menuBarHelp:
        '시스템 트레이에 Nagori 아이콘을 표시합니다 (macOS: 메뉴 막대 / Windows: 알림 영역 / Linux: 상태 표시기). 백그라운드 전용으로 사용하려면 비활성화하세요.',
      clearOnQuit: '종료 시 고정되지 않은 기록 삭제',
      clearOnQuitHelp:
        '앱이 종료될 때 고정되지 않은 모든 항목을 제거합니다. 고정된 항목은 보존됩니다.',
    },
    display: {
      legend: '팔레트 표시',
      rowCount: '표시 행 수',
      rowCountHelp: '스크롤 전 팔레트에 표시할 최대 행 수(3–20).',
      previewPane: '미리보기 패널 표시',
      previewPaneHelp: '비활성화하면 결과 목록이 전체 너비를 차지해 팔레트가 컴팩트해집니다.',
    },
    hotkeys: {
      legend: '단축키',
      paletteHeading: '팔레트 단축키',
      paletteHelp: '팔레트 내 단축키를 재정의합니다. 비워 두면 기본값이 유지됩니다.',
      secondaryHeading: '보조 전역 단축키',
      secondaryHelp: '메인 팔레트 단축키와 함께 등록되는 선택적인 시스템 전역 단축키입니다.',
      placeholder: '클릭해 기록',
      recordingHint: '단축키를 누르세요… (Esc로 취소)',
      clearAriaLabel: '단축키 지우기',
      paletteActions: {
        pin: '선택 항목 고정/해제',
        delete: '선택 항목 삭제',
        'paste-as-plain': '일반 텍스트로 붙여넣기',
        'copy-without-paste': '붙여넣기 없이 복사만',
        clear: '검색어 지우기',
        'open-preview': '확장 미리보기 토글',
      },
      secondaryActions: {
        'repaste-last': '가장 최근 항목 다시 붙여넣기',
        'clear-history': '고정되지 않은 기록 삭제',
      },
    },
    updates: {
      legend: '업데이트',
      autoCheck: '자동으로 업데이트 확인',
      channel: '채널',
      checkNow: '지금 확인',
      checking: '확인 중…',
      upToDate: '최신 릴리스를 사용 중입니다.',
      available: '업데이트 사용 가능: {version}',
      availableManual:
        '업데이트 사용 가능: {version}. 현재 설치 형식에서는 자동 업데이트를 사용할 수 없습니다. GitHub에서 새 빌드를 다운로드하세요.',
      viewRelease: '릴리스 보기',
      downloadManual: 'GitHub에서 다운로드',
    },
    capabilities: {
      legend: '플랫폼 기능',
      help: 'Nagori가 현재 OS에서 사용할 수 있는 기능 목록입니다. "권한 필요"로 표시된 기능은 OS의 시스템 설정에서 액세스를 허용하면 사용할 수 있습니다.',
      platform: '플랫폼',
      tier: '계층',
      openSetup: '설정 열기',
      columns: { capability: '기능', status: '상태', detail: '세부 정보' },
      statuses: {
        available: '사용 가능',
        unsupported: '미지원',
        requiresPermission: '권한 필요',
        requiresExternalTool: '외부 도구',
        experimental: '실험적',
      },
      rows: {
        captureText: '텍스트 캡처',
        captureImage: '이미지 캡처',
        captureFiles: '파일 캡처',
        writeText: '텍스트 쓰기',
        writeImage: '이미지 쓰기',
        clipboardMultiRepresentationWrite: '다중 표현 쓰기',
        autoPaste: '자동 붙여넣기',
        globalHotkey: '전역 단축키',
        frontmostApp: '최전면 앱 감지',
        permissionsUi: '권한 UI',
        updateCheck: '업데이트 확인',
        previewQuickLook: '미리보기 (Quick Look)',
      },
    },
  },
  keybindings: {
    selectNext: '다음 결과',
    selectPrev: '이전 결과',
    selectFirst: '처음으로',
    selectLast: '끝으로',
    confirm: '선택 항목 붙여넣기',
    openActions: '빠른 동작 열기',
    togglePin: '고정 / 해제',
    delete: '삭제',
    openSettings: '설정 열기',
    close: '닫기',
  },
  time: {
    justNow: '방금',
    minutesAgo: (n: number): string => `${n}분 전`,
    hoursAgo: (n: number): string => `${n}시간 전`,
    daysAgo: (n: number): string => `${n}일 전`,
  },
  errors: {
    unknown: '알 수 없는 오류가 발생했습니다.',
    storage: '저장소 오류가 발생했습니다.',
    search: '검색 오류가 발생했습니다.',
    platform: 'OS 연동에 실패했습니다.',
    permission: '권한이 부족합니다.',
    ai: '빠른 동작에서 오류가 발생했습니다.',
    policy: '정책에 의해 동작이 차단되었습니다.',
    notFound: '찾을 수 없습니다.',
    invalidInput: '입력이 올바르지 않습니다.',
    unsupported: '이 플랫폼에서는 지원되지 않습니다.',
    configuration: '구성 오류입니다. 빌드 결함이므로 이슈로 보고해 주세요.',
  },
  locales: {
    system: '시스템 (OS 따름)',
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
    autoPasteFailedTitle: '자동 붙여넣기에 실패했습니다',
    autoPasteFailedFallback: '자동 붙여넣기에 실패했습니다.',
    hotkeyRegisterFailedTitle: '단축키를 사용할 수 없습니다',
    hotkeyRegisterFailedFallback: '설정된 글로벌 단축키 등록에 실패했습니다.',
    openSettings: '설정 열기',
    dismiss: '닫기',
    accessibilityGrantedTitle: 'Accessibility 권한이 허용되었습니다',
  },
};

import type { Messages } from './en';

export const ja: Messages = {
  palette: {
    placeholder: '履歴を検索…',
    searching: '検索中…',
    resultCount: (count: number): string => `${count.toLocaleString('ja')} 件`,
    elapsed: (ms: number): string => `${ms.toFixed(0)} ms`,
    empty: '履歴がまだありません。',
    fallback: '（Tauri ランタイム未起動）ここに最近コピーした項目が並びます。',
    hints: {
      navigate: '選択',
      paste: 'ペースト',
      actions: 'アクション',
      settings: '設定',
    },
    filters: {
      toolbarLabel: 'クイックフィルタ',
      today: '今日',
      last7days: '過去7日',
      pinned: 'ピン留め',
    },
  },
  preview: {
    empty: 'プレビューする項目を選択してください。',
    loading: 'プレビューを読み込み中…',
    truncated: 'プレビューは途中まで表示しています。',
    truncation: {
      headOnly: ({ shown, total }: { shown: string; total: string }): string =>
        `${total} のうち先頭 ${shown} を表示しています。`,
      headAndTail: ({ elided }: { elided: string }): string =>
        `先頭と末尾を表示しています。中間 ${elided} を省略しました。`,
      elidedMatch: '検索一致が省略部分にあります。',
      expand: '全文を表示',
      expanding: '全文を読み込み中…',
    },
    fields: {
      id: 'ID',
      sensitivity: '機密度',
      source: '送信元',
      size: 'サイズ',
      rank: 'ランク',
      formats: '保持された形式',
    },
    none: '—',
    summary: {
      lines: (count: number): string => `${count.toLocaleString('ja')} 行`,
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
      loading: '画像を読み込み中…',
      unavailable: '画像を表示できません。',
      alt: 'クリップボード画像のプレビュー',
    },
    fileList: {
      summary: (shown: number, total: number): string =>
        total === shown
          ? `${total.toLocaleString('ja')} 件`
          : `${shown.toLocaleString('ja')} / ${total.toLocaleString('ja')} 件`,
      moreFiles: (count: number): string => `他 ${count.toLocaleString('ja')} 件`,
      inFolder: (prefix: string): string => `${prefix} 配下`,
    },
    url: {
      punycodeBadge: 'punycode',
      punycodeBadgeTitle: ({ ascii }: { ascii: string }): string =>
        `IDN ホスト名。ASCII 表記: ${ascii}`,
      openHint: 'Enter で開く',
      confirmTitle: 'このリンクを開きますか？',
      confirmDescription: ({ host }: { host: string }): string =>
        `既定のブラウザで ${host} を開きます。`,
      confirm: '開く',
      cancel: 'キャンセル',
      openFailed: 'URL を開けませんでした。',
    },
  },
  status: {
    captureOn: '取り込み有効',
    capturePaused: '取り込み一時停止',
    entryCount: (n: number): string => `${n.toLocaleString('ja')} 件`,
    selectedCount: (n: number): string => `${n.toLocaleString('ja')} 件選択中`,
  },
  actionMenu: {
    title: 'クイックアクション',
    actions: {
      Summarize: '要約',
      FormatJson: 'JSON 整形',
      ExtractTasks: 'タスク抽出',
      RedactSecrets: '秘匿情報マスク',
    },
    tauriRequired: 'クイックアクションには Tauri ランタイムが必要です。',
    resultTitle: '実行結果',
    copyResult: 'コピー',
    copied: 'コピーしました',
    saveResult: '新しいエントリとして保存',
    saved: '保存しました',
    closeResult: '閉じる',
    runFailed: 'クイックアクションの実行に失敗しました。',
  },
  onboarding: {
    title: 'Nagori のセットアップを完了する',
    description: '一部の機能は macOS の追加権限が必要です。',
    accessibilityRequired: 'アクセシビリティ権限が必要です',
    accessibilityHint:
      'システム設定 → プライバシーとセキュリティ → アクセシビリティ で Nagori を許可するとアクティブなアプリへ自動ペーストできます。',
    autoPasteDisabled:
      'アクセシビリティ未許可のため自動ペーストは無効です。Enter ではクリップボードへコピーのみ行います。',
    notificationsHint:
      '取り込み一時停止や自動ペースト失敗の通知を受け取るには通知を許可してください。',
    openSettings: 'システム設定を開く',
    dismiss: 'あとで設定する',
  },
  settings: {
    title: '設定',
    backToPalette: 'パレットへ戻る',
    loading: '読み込み中…',
    statusSaving: '保存中…',
    statusSaved: '保存しました',
    statusError: '保存に失敗しました: {error}',
    tauriRequired: '設定の保存には Tauri ランタイムが必要です。',
    tabs: {
      general: '一般',
      privacy: 'プライバシー',
      cli: 'CLI',
      advanced: '詳細',
    },
    capture: {
      legend: 'キャプチャ',
      enabled: 'クリップボードの取り込みを有効にする',
      autoPaste: 'Enter で自動ペーストする',
      pasteFormatDefault: '既定のペースト形式',
      pasteFormatOptions: {
        preserve: '元の形式',
        plain_text: 'プレーンテキスト',
      },
      hotkey: 'グローバルホットキー',
      captureInitialClipboard: '起動時にクリップボードを取り込む',
      captureInitialClipboardHelp:
        '有効にすると起動時点でのクリップボードを履歴に追加します。無効にすると既存の内容は無視されます。',
    },
    retention: {
      legend: '保持ポリシー',
      maxCount: '最大件数',
      maxDays: '保持日数',
      maxDaysPlaceholder: '0 で無期限',
      maxDaysHelp: '0 を指定すると履歴を期限なく保持します。',
      maxTotalBytes: '総ストレージ上限',
      maxTotalBytesPlaceholder: '0 で無制限',
      maxTotalBytesHelp: 'ピン留めした項目は上限超過時も削除されません。',
      maxBytes: '1 件あたりの最大バイト数',
      pasteDelayMs: 'ペースト遅延（ms）',
    },
    privacy: {
      legend: 'フィルタ',
      appDenylist: 'アプリ拒否リスト',
      appDenylistHelp: '1 行に 1 つアプリ名を記入。これらのアプリからのコピーは取り込みません。',
      regexDenylist: '正規表現拒否リスト',
      regexDenylistHelp:
        '1 行に 1 つパターンを記入（例: INTERNAL-\\d+）。一致した内容は履歴に保存されません。各パターンは 256 バイト（UTF-8）以内、エスケープしていない ( ) の入れ子は 3 段までに収めてください。複雑なルールは入れ子にせず、複数行に分けて書きます。',
      secretHandling: 'シークレットの扱い',
      secretHandlingHelp:
        'API キー・JWT・秘密鍵などシークレットと判定されたクリップを保存する際の動作。',
      secretHandlingOptions: {
        block: '保存しない（拒否）',
        store_redacted: 'マスク済みで保存（既定）',
        store_full: 'そのまま保存（プレビューはマスク）',
      },
      captureKinds: '取り込み対象',
      captureKindsHelp: '無効にした種類はシークレット判定の前に除外されます。',
      captureKindOptions: {
        text: 'テキスト',
        url: 'URL',
        code: 'コード',
        image: '画像',
        fileList: 'ファイル',
        richText: 'リッチテキスト',
        unknown: '不明',
      },
      storeFullWarning:
        '警告: 「そのまま保存」を選ぶと、API キー・JWT・秘密鍵などのシークレットがローカル SQLite に平文で残ります。DB は暗号化されていないため、ホームディレクトリへの読み取りアクセス（バックアップ、同期クライアント、マルウェアなど）から復元される恐れがあります。リスクを理解できない限り「マスク済みで保存」を推奨します。',
      storeFullConfirm:
        'シークレットを平文で保存しますか？ DB は暗号化されておらず、データディレクトリのバックアップを含めディスク上から原文を取り出せる状態になります。',
      regexDenylistAutosaveHint: '強調表示された正規表現エラーを修正すると自動保存されます。',
      regexErrors: {
        lineLabel: '{line} 行目:',
        tooLong:
          '長すぎます（{bytes} バイト > {limit}）。複数行に分割するか、使っていない選択肢を削ってください。',
        tooNested:
          '括弧の入れ子が {depth} で上限 {limit} を超えています。`(?: … )` を 1 つだけ使うなどフラットにするか、複数行に分けてください。',
        invalidSyntax:
          '正規表現の構文エラー: {error}。リテラルのメタ文字は \\\\ でエスケープするか、書き直してください。',
        empty: '空のエントリです。空行を削除するか、パターンを入力してください。',
      },
    },
    cli: {
      legend: 'CLI',
      ipcEnabled: 'CLI からの IPC 接続を許可する',
    },
    appearance: {
      legend: '表示',
      locale: '言語',
      theme: 'テーマ',
      themeOptions: {
        system: 'システム',
        light: 'ライト',
        dark: 'ダーク',
      },
      recentOrder: '履歴の並び順',
      recentOrderOptions: {
        by_recency: '新しい順',
        by_use_count: '使用回数順',
        pinned_first_then_recency: 'ピン留め優先',
      },
    },
    integration: {
      legend: 'OS 連携',
      autoLaunch: 'ログイン時に自動起動',
      autoLaunchHelp:
        'OS の自動起動機能 (macOS: LaunchAgent / Windows: Run レジストリ / Linux: XDG autostart) を使ってログイン時に Nagori を起動します。',
      menuBar: 'トレイアイコンを表示',
      menuBarHelp:
        'Nagori のトレイアイコンを表示します (macOS: メニューバー / Windows: 通知領域 / Linux: ステータスインジケーター)。バックグラウンド常駐のみで使う場合は無効にできます。',
      clearOnQuit: '終了時にピン留め以外を削除',
      clearOnQuitHelp:
        'アプリ終了時にピン留めしていない履歴をすべて削除します。ピン留めした項目は残ります。',
    },
    display: {
      legend: 'パレット表示',
      rowCount: '表示行数',
      rowCountHelp: 'スクロール前にパレットへ表示する最大行数（3〜20）。',
      previewPane: 'プレビューペインを表示',
      previewPaneHelp: '無効にするとリストが横幅いっぱいに広がり、パレットがコンパクトになります。',
    },
    hotkeys: {
      legend: 'ホットキー',
      paletteHeading: 'パレット内ショートカット',
      paletteHelp: 'パレット内のショートカットを上書きできます。空欄の場合は既定値が使われます。',
      secondaryHeading: '追加グローバルホットキー',
      secondaryHelp:
        'メインのパレットホットキーと並行して登録される、任意のシステム全域ショートカットです。',
      placeholder: '例: Cmd+Shift+P',
      paletteActions: {
        pin: '選択をピン留め切替',
        delete: '選択を削除',
        'paste-as-plain': 'プレーンテキストでペースト',
        'copy-without-paste': 'ペーストせずコピーのみ',
        clear: '検索クエリをクリア',
        'open-preview': '拡大プレビューを開閉',
      },
      secondaryActions: {
        'repaste-last': '直近のエントリを再ペースト',
        'clear-history': 'ピン留め以外の履歴を削除',
      },
    },
    updates: {
      legend: 'アップデート',
      autoCheck: '自動でアップデートを確認',
      channel: 'チャネル',
      checkNow: '今すぐ確認',
      checking: '確認中…',
      upToDate: '最新バージョンを使用しています。',
      available: '新しいバージョンがあります: {version}',
      availableManual:
        '新しいバージョンがあります: {version}。現在のインストール形態では自動更新ができないため、GitHub から新しいビルドをダウンロードしてください。',
      viewRelease: 'リリースを表示',
      downloadManual: 'GitHub からダウンロード',
    },
    capabilities: {
      legend: 'プラットフォーム機能',
      help: 'Nagori が現在の OS で利用できる機能の一覧です。「要許可」と表示されている機能は、OS のシステム設定でアクセスを許可すると使えるようになります。',
      platform: 'プラットフォーム',
      tier: 'ティア',
      columns: { capability: '機能', status: '状態', detail: '詳細' },
      statuses: {
        available: '利用可能',
        unsupported: '非対応',
        requiresPermission: '要許可',
        requiresExternalTool: '外部ツール',
        experimental: '実験的',
      },
      rows: {
        captureText: 'テキストを取り込み',
        captureImage: '画像を取り込み',
        captureFiles: 'ファイルを取り込み',
        writeText: 'テキストを書き込み',
        writeImage: '画像を書き込み',
        clipboardMultiRepresentationWrite: '複数表現での書き戻し',
        autoPaste: '自動ペースト',
        globalHotkey: 'グローバルホットキー',
        frontmostApp: '最前面アプリの取得',
        permissionsUi: '権限設定 UI',
        updateCheck: 'アップデート確認',
        previewQuickLook: 'プレビュー (Quick Look)',
      },
    },
  },
  keybindings: {
    selectNext: '次の候補へ',
    selectPrev: '前の候補へ',
    selectFirst: '先頭へ',
    selectLast: '末尾へ',
    confirm: '選択をペースト',
    openActions: 'クイックアクション',
    togglePin: 'ピン留め切替',
    delete: '削除',
    openSettings: '設定を開く',
    close: '閉じる',
  },
  time: {
    justNow: '今',
    minutesAgo: (n: number): string => `${n}分前`,
    hoursAgo: (n: number): string => `${n}時間前`,
    daysAgo: (n: number): string => `${n}日前`,
  },
  errors: {
    unknown: '未知のエラーが発生しました。',
    storage: 'ストレージのエラーが発生しました。',
    search: '検索エラーが発生しました。',
    platform: 'OS との連携に失敗しました。',
    permission: '権限が不足しています。',
    ai: 'クイックアクションでエラーが発生しました。',
    policy: 'ポリシーによって操作がブロックされました。',
    notFound: '対象が見つかりません。',
    invalidInput: '入力内容が無効です。',
    unsupported: 'このプラットフォームでは未対応です。',
    configuration: '設定エラー（ビルド側の不具合）です。Issue として報告してください。',
  },
  locales: {
    system: 'システム（OS に追従）',
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
    autoPasteFailedTitle: '自動ペーストに失敗しました',
    autoPasteFailedFallback: '自動ペーストに失敗しました。',
    openSettings: '設定を開く',
    dismiss: '閉じる',
  },
};

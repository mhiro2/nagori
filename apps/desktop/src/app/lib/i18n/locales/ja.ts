import type { Messages } from './en';

export const ja: Messages = {
  palette: {
    placeholder: '履歴を検索…',
    searching: '検索中…',
    resultCount: (count: number): string => `${count.toLocaleString('ja')} 件`,
    elapsed: (ms: number): string => `${ms.toFixed(0)} ms`,
    empty: '履歴がまだありません。',
    fallback: '（Tauri ランタイム未起動）ここに最近コピーした項目が並びます。',
    screenshotBadge: 'スクショ',
    hints: {
      navigate: '選択',
      paste: 'ペースト',
      pin: 'ピン留め',
      actions: 'アクション',
      settings: '設定',
      preview: 'プレビュー',
    },
    filters: {
      toolbarLabel: 'クイックフィルタ',
      today: '今日',
      yesterday: '昨日',
      last7days: '過去7日',
      last30days: '過去30日',
      pinned: 'ピン留め',
      kindText: 'テキスト',
      kindUrl: 'URL',
      kindCode: 'コード',
      kindImage: '画像',
      kindFiles: 'ファイル',
      dateGroup: '日付',
      typeGroup: '種類',
      sourceGroup: 'コピー元アプリ',
      sourceShort: 'アプリ',
      allApps: 'すべてのアプリ',
      clear: 'フィルタをクリア',
    },
    fileList: {
      more: (overflow) => `+${overflow.toLocaleString('ja')}`,
      locations: (count) => `${count.toLocaleString('ja')} か所`,
      rowAria: ({ total, names, location }) => {
        const head = total === 1 ? names : `${total.toLocaleString('ja')} 件のファイル: ${names}`;
        return location ? `${head}、${location} 配下` : head;
      },
    },
  },
  rankReason: {
    exact: '完全一致',
    prefix: '前方一致',
    substring: '部分一致',
    fullText: '全文',
    fuzzy: 'あいまい',
    semantic: '意味',
    recent: '最近',
    frequent: 'よく使う',
    pinned: 'ピン留め',
  },
  preview: {
    empty: 'プレビューする項目を選択してください。',
    loading: 'プレビューを読み込み中…',
    details: '詳細',
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
    },
    additionalData: 'その他のクリップボードデータ',
    clipboardCategory: { image: '画像', text: 'テキスト', files: 'ファイル' },
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
      location: '場所',
      fileRowAria: ({ name, location }: { name: string; location: string | null }): string =>
        location ? `${name}、${location} 配下` : name,
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
    autoPasteOff: '自動ペースト OFF — Accessibility 未許可',
    autoPasteOffShort: '⚠ 自動ペースト OFF',
    autoPasteOffSetupAria: '自動ペースト OFF: Accessibility の許可が必要です。セットアップを開く。',
    pasteDiagnostics: {
      label: '⚠ 自動ペースト失敗',
      toolFallback: 'ペーストツール',
      hint: {
        accessibilityMissing:
          '自動ペースト失敗: Accessibility の許可が必要です。コピー済み — 手動で貼り付けてください。',
        toolMissing: ({ tool }) =>
          `自動ペースト失敗: ${tool} が未インストールです。コピー済み — ${tool} を導入するか手動で貼り付けてください。`,
        timeout:
          '自動ペーストがタイムアウトしました（コンポジタが応答していない可能性）。コピー済み — 手動で貼り付けるか再試行してください。',
        synthUnsupported:
          'このプラットフォームでは自動ペーストを利用できません。コピー済み — 手動で貼り付けてください。',
        previousAppLost:
          '自動ペーストをスキップ: 元のアプリに復帰できませんでした。コピー済み — 手動で貼り付けてください。',
        unknown: '自動ペースト失敗。コピー済み — 手動で貼り付けてください。',
      },
    },
  },
  actionMenu: {
    title: 'クイックアクション',
    close: '閉じる',
    actions: {
      SummarizeFirstSentence: '要約（先頭の一文）',
      FormatJson: 'JSON 整形',
      ExtractTasks: 'タスク抽出',
      RedactSecrets: '秘匿情報マスク',
    },
    aiActions: {
      Summarize: '要約',
      Rewrite: '書き直し',
      FormatMarkdown: 'Markdown 整形',
      ExtractTasks: 'タスクを整理',
      ExplainCode: 'コード解説',
    },
    aiBadge: 'AI',
    aiCancel: 'キャンセル',
    aiUnavailable: '現在 AI アクションは利用できません。',
    notApplicable: {
      image: '画像には適用できません。',
      fileList: 'ファイルには適用できません。',
      url: 'この操作は URL には適用できません。',
    },
    aiRemediation: {
      'ai.unavailable.apple_intelligence_not_enabled':
        'AI アクションを使うには、システム設定で Apple Intelligence を有効化してください。',
      'ai.unavailable.device_not_eligible':
        'この Mac は Apple Intelligence に対応していません（Apple シリコンが必要です）。',
      'ai.unavailable.model_not_ready':
        'オンデバイスモデルをダウンロード中です。しばらくしてから再試行してください。',
      'ai.unavailable.asset_missing': '必要なオンデバイスアセットが利用できません。',
      'ai.unavailable.rate_limited':
        'オンデバイスモデルが混雑しています。しばらくしてから再試行してください。',
    },
    tauriRequired: 'クイックアクションには Tauri ランタイムが必要です。',
    generating: '生成中…',
    working: '処理中…',
    done: '完了',
    resultTitle: '実行結果',
    copyResult: 'コピー',
    copied: 'コピーしました',
    saveResult: '新しいエントリとして保存',
    saved: '保存しました',
  },
  pastePicker: {
    title: 'ペースト形式',
    keepOriginal: '元の形式のまま',
    categories: {
      files: 'ファイル',
      image: '画像',
      plainText: 'プレーンテキスト',
      html: 'HTML',
      richText: 'リッチテキスト',
    },
  },
  setup: {
    title: 'Nagori をセットアップ',
    intro:
      'Nagori が他のアプリへ自動ペーストするために必要な権限を許可します。あとから システム設定 で変更できます。',
    accessibility: {
      title: 'アクセシビリティ',
      required: '必須',
      description:
        'アクセシビリティを有効にすると、Nagori が履歴をフォーカス中のアプリへ直接ペーストできます。「アクセシビリティを許可…」を押して macOS のダイアログを表示し、Nagori のスイッチをオンにしてください。',
      descriptionLinux:
        'Wayland セッションで `wtype` パッケージをインストールすると、Nagori がフォーカス中のアプリへ Ctrl+V を合成できます。',
      descriptionWindows:
        'Windows では、アクセシビリティ相当の権限なしで Nagori がフォーカス中のアプリへペーストできます。ここで設定する項目はありません。',
      screenshotAlt:
        'システム設定 → プライバシーとセキュリティ → アクセシビリティ で Nagori のトグルを強調表示したスクリーンショット。',
      grantButton: 'アクセシビリティを許可…',
      grantButtonRetry: 'システム設定を開く',
      recheckButton: '再確認',
      requesting: '要求中…',
      states: {
        NotRequested: '未要求',
        PromptShownNotGranted: '対応が必要',
        Granted: '許可済み',
        RevokedAfterGranted: '再有効化',
        Unavailable: '対象外',
      },
      statusLabel: '状態',
      messages: {
        NotRequested:
          'Nagori はまだアクセシビリティを要求していません。下のボタンを押すと macOS のダイアログが表示されます。',
        PromptShownNotGranted:
          'macOS は二度目のダイアログを表示しません。システム設定を開き、アクセシビリティ一覧で Nagori をオンにしてください。',
        Granted: '自動ペーストが利用可能です。',
        RevokedAfterGranted:
          '以前は許可されていました。システム設定で再度有効にすると自動ペーストが復活します。',
        UnavailableMacosFallback: 'このビルドではアクセシビリティの状態を取得できません。',
        UnavailableWindows: 'Windows では自動ペーストにアクセシビリティ相当の権限は不要です。',
        UnavailableLinux:
          'Linux の自動ペーストは `wtype` ヘルパーに依存します。パッケージマネージャでインストールしてください。',
      },
      timeoutError:
        '60 秒以内に許可を検知できませんでした。システム設定 → プライバシーとセキュリティ → アクセシビリティ で Nagori のスイッチを確認し、「再確認」を押してください。',
      requestError:
        'アクセシビリティ要求を開始できませんでした。コンソール.app で詳細を確認してください。',
    },
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
      setup: 'セットアップ',
      general: '一般',
      privacy: 'プライバシー',
      ai: 'AI',
      cli: 'CLI',
      advanced: '詳細',
    },
    ai: {
      legend: 'AI',
      enabled: 'AI アクションを有効化',
      enabledHelp:
        '要約などのモデル連携アクションは Apple Intelligence によりデバイス上で実行されます。既定では無効です。',
      provider: 'プロバイダ',
      providerDisabled: '無効',
      providerApple: 'Apple（オンデバイス）',
      allowStreaming: '生成中の結果をストリーミング表示',
      allowStreamingHelp:
        'モデルの生成中に途中経過を表示します。オフにすると最終結果のみ表示します。',
      semanticIndex: 'セマンティック検索インデックス',
      semanticIndexHelp:
        'オンデバイスの埋め込みを作成し、本文だけでなく意味でも検索できるようにします。Apple のオンデバイス埋め込みモデル (macOS) を使用し、既定はオフです。',
      semanticIndexAcPowerOnly: '電源接続中のみインデックスを作成',
      semanticIndexAcPowerOnlyHelp:
        'バッテリー駆動中はバックグラウンドの埋め込み生成を一時停止して節電します。オフにするとバッテリー駆動中も作成します。',
      semanticIndexRebuild: 'インデックスを再構築',
      semanticIndexStatus: 'インデックスの状態',
      semanticIndexStateReady: '最新の状態',
      semanticIndexStateIndexing: 'インデックス作成中…',
      semanticIndexStatePaused: '一時停止（バッテリー駆動中）',
      semanticIndexStateUnavailable: '埋め込みモデルを利用できません',
      semanticIndexStateUnsupported: 'このデバイスでは非対応',
      semanticIndexStateDisabled: '無効',
      status: '利用可否',
      statusAvailable: '利用可能',
      statusUnavailable: '利用不可',
      statusDisabled: '無効',
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
      appDenylistPasswordManagers: 'パスワードマネージャをブロック',
      appDenylistPasswordManagersHelp:
        '同梱プリセット（1Password / Bitwarden / KeePassXC / Apple Passwords）からのコピーを正確なアプリ識別子で除外します。プリセットの内容は固定で編集できません。クリップボード経由でこれらから貼り付ける必要がない限り、有効のままにしておくことを推奨します。',
      appDenylistPatterns: 'カスタムパターン',
      appDenylistPatternsHelp:
        '1 行に 1 つ部分文字列を記入してください。送信元アプリ名・バンドル ID・実行ファイルパスのいずれかに含まれる場合は取り込みません（大文字小文字は区別しません）。プリセットに無いアプリ（Dashlane / LastPass / 社内ツール等）をブロックしたい場合にここへ追加してください。',
      appDenylistUnsupported:
        'このデスクトップ環境は最前面のアプリ情報を提供しないため、アプリ単位のブロックは機能しません。下の正規表現拒否リストや「取り込む種類」で取り込み範囲を制限してください。',
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
      install: {
        legend: 'コマンドラインツール',
        help: '同梱の `nagori` コマンドラインツールを ~/.local/bin にインストールすると、ターミナルから履歴の検索・貼り付けができます。',
        button: 'nagori CLI をインストール',
        reinstall: '再インストール',
        installing: 'インストール中…',
        statusInstalled: 'nagori は {path} にリンクされています。',
        statusNotInstalled: 'nagori コマンドラインツールはまだインストールされていません。',
        installed: 'nagori を {path} にインストールしました。',
        installedNeedsPath:
          'nagori を {path} にインストールしました。利用するには下のディレクトリを PATH に追加してください。',
        notOnPath:
          '{dir} はまだ PATH に含まれていません。次の行をシェルの設定（例: ~/.zshrc）に追加し、新しいターミナルを開いてください:',
        pathExport: 'export PATH="$HOME/.local/bin:$PATH"',
        unavailable: '同梱の CLI はパッケージ版アプリにのみ含まれ、開発ビルドには含まれません。',
        unsupported:
          'このプラットフォームではワンクリックインストールに対応していません。同梱の nagori 実行ファイルを PATH 上のディレクトリにコピーしてください。',
      },
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
      paletteHelp:
        'パレット内で使用するショートカットです。変更していない項目には既定のキーが使われます。',
      secondaryHeading: '追加グローバルホットキー',
      secondaryHelp:
        'パレットを開かずに実行できる追加のグローバルショートカットです。必要な項目だけ設定してください。未設定の項目は無効です。',
      placeholder: 'ショートカットを設定',
      recordingHint: 'キーを入力…',
      recordingCancelHint: 'Esc でキャンセル',
      clearAriaLabel: 'ショートカットをクリア',
      defaultMarker: '既定',
      disabledMarker: '無効',
      notSet: '未設定',
      reset: 'リセット',
      fieldAriaLabel: '{action}のショートカット',
      restoreDefault: '{action} を既定に戻す',
      removeShortcut: '{action} の割り当てを解除',
      disableShortcut: '{action} を無効にする',
      paletteActions: {
        pin: 'ピン留めを切り替える',
        delete: '選択中の項目を削除',
        'paste-as-plain': '形式を選んでペースト',
        'copy-without-paste': 'クリップボードにコピー',
        clear: '検索をクリア',
        'open-preview': 'プレビューを拡大表示',
      },
      secondaryActions: {
        'repaste-last': '直近の項目を再ペースト',
        'clear-history': 'ピン留め以外をすべて削除',
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
      openSetup: 'セットアップを開く',
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
    internal: '問題が発生しました。もう一度お試しください。',
    forbidden: 'この項目では利用できません。',
    paste: '自動ペーストに失敗しました。コピー済み — 手動で貼り付けてください。',
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
    hotkeyRegisterFailedTitle: 'ホットキーを利用できません',
    hotkeyRegisterFailedFallback: '設定されたグローバルホットキーの登録に失敗しました。',
    openSettings: '設定を開く',
    dismiss: '閉じる',
    accessibilityGrantedTitle: 'Accessibility を許可しました',
  },
};

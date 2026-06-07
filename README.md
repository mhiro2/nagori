<p align="center">
  <img src="./assets/hero.jpg" alt="Nagori — Local-first clipboard history and memory" width="100%" />
</p>

## Features

- Persistent clipboard history (text, images, file lists) stored locally in SQLite.
- Full-text search and pinning from a desktop palette or the `nagori` CLI.
- Japanese / CJK partial-match search: kana-insensitive (a Katakana clip is
  found by a Hiragana query and vice versa) with single-kanji recall, on top of
  full-text and ASCII fuzzy matching.
- Recall-oriented result rows: a language badge on code (JSON / SQL / Rust /
  …, detected on-device with the same canonical id the preview highlighter
  uses), a strong-brand badge on known URLs (GitHub / YouTube / …, from the
  hostname alone — no network), and pixel dimensions, file size, and a
  *Screenshot* hint on image rows so "the screenshot I just took" is
  scannable in the list.
- Built-in secret classifier that redacts API keys, JWTs, AWS / GitHub tokens,
  PEM blocks, credit-card numbers, and OTPs before they hit disk.
- User regex denylist for project-specific patterns.
- Auto-paste back into the previously focused window (Cmd/Ctrl+V synthesis).
- Quick actions on a selected entry: summarise, format JSON, extract tasks,
  redact secrets — all computed locally without any network calls.
- macOS on-device AI actions (opt-in, off by default): summarise, rewrite,
  reformat Markdown, extract tasks, explain code, and translate — backed by
  Apple's on-device frameworks (Foundation Models for text generation, the
  Translation framework for translate). Inference runs entirely on-device:
  no clipboard text leaves the machine and the Private Cloud Compute path is
  not used.
- Optional on-device semantic search (opt-in, macOS): matches entries by
  meaning using Apple's `NLContextualEmbedding`, indexed locally with
  `sqlite-vec`.
- URL preview shows host on its own row with a punycode badge when the
  displayed Unicode host differs from its ASCII form; press **Enter** in
  the expanded preview to open the URL in the default browser after a
  confirm dialog (Public entries only, `https` / `http` only).
- Image preview uses a daemon-cached 512px thumbnail on row navigation so
  the palette stays responsive on multi-megabyte screenshots, with a
  `dimensions · format · size` summary chip. **⌘/Ctrl E** opens the
  full-width expanded preview, which switches to the original payload and
  zooms toward the pointer — pinch the trackpad (or **⌘/Ctrl** + scroll),
  double-click to toggle fit ↔ 2×, or use the keyboard (**⌘/Ctrl** with
  **+** / **−** / **0**); a zoomed image pans with the scrollbar / trackpad.
- Long-text preview shows head and tail with a middle-elided marker so
  the end of large logs / pastes stays visible; when the active search
  hit lands inside the elided range the preview flags it so you can
  expand to the 1 MiB full view (Public entries only).
- Quick Look preview on macOS — Cmd+Y in the palette opens the
  highlighted entry in the system Quick Look overlay (Public entries
  only). Windows and Linux Wayland surface the capability as
  Unsupported because neither OS ships a comparable system overlay.
- Bundled per-OS release artefacts with an in-app update-availability probe.

## Install

> **Canary / pre-1.0.** The `0.0.x` line is a dogfooding canary. Bundles
> are published as GitHub pre-releases and the in-app updater only
> probes for new versions — it does not auto-install. Expect rough
> edges; see [Known limitations](#known-limitations) before installing.

| Platform                  | Bundle                                                       |
| ------------------------- | ------------------------------------------------------------ |
| macOS 26+ (arm64 / x86_64) | Unsigned `.app` / `.dmg` from [GitHub Releases](https://github.com/mhiro2/nagori/releases) (Gatekeeper warns on first launch) |
| Windows 10 1809+ / 11 (x86_64) | Unsigned NSIS installer (SmartScreen warns on first launch) |
| Linux Wayland (x86_64)    | `.deb` and `AppImage`                                        |

macOS bundles declare `LSMinimumSystemVersion = 26.0` and the installer
refuses to launch on earlier releases — the 0.0.x line is validated
only against Tahoe. Linux additionally requires the `wtype` binary on
`$PATH` for auto-paste and a Wayland compositor that exposes
`wlr_data_control` or `ext_data_control` (sway, Hyprland, KDE Plasma
5.27+, …). See [`docs/platforms.md`](./docs/platforms.md) for the full
compatibility matrix and troubleshooting.

### Known limitations

- **macOS `.app` / `.dmg` are unsigned.** Gatekeeper warns on first
  launch; right-click → **Open** (or `xattr -d com.apple.quarantine`)
  to proceed.
- **Windows NSIS bundle is unsigned.** SmartScreen warns on first
  launch until an Authenticode EV certificate is in place; choose
  **More info → Run anyway** to proceed.
- **Linux GNOME Wayland is not supported.** GNOME exposes neither
  `wlr_data_control` nor `ext_data_control`, so the clipboard cannot
  be captured. X11 sessions are also out of scope.
- **Linux global hotkeys do not register on pure Wayland.**
  `tauri-plugin-global-shortcut` is X11-only upstream — toggle the
  palette from the tray icon instead.
- **In-app updater is read-only.** The desktop shell probes
  `latest.json` and surfaces "View release" / "Download manually"
  copy, but does not call `download_and_install`. Upgrade by
  downloading the bundle from the GitHub release page.
- **Auto-paste fallback is manual.** When the synthesised Cmd/Ctrl+V
  fails (restore-target lost, target app refused the paste, …), the
  entry is still copied to the clipboard and a `paste_failed` toast
  prompts you to paste manually — there is no automatic retry. The
  failure is classified (Accessibility missing, paste tool missing,
  timeout, source-app lost, …) and the palette's status bar keeps a
  persistent diagnostic chip whose tooltip names the fix (e.g. "install
  `wtype`"); it clears on the next successful paste or by clicking it.
  The missing-Accessibility case folds into the dedicated Accessibility
  warning chip, which carries a *Setup* shortcut.

## Usage

1. Launch the desktop app — it starts the background daemon and registers a
   global hotkey (default `Ctrl+Shift+V`, `Cmd+Shift+V` on macOS). On Linux
   Wayland the upstream global-shortcut plugin is X11-only, so the registration
   fails and you toggle the palette from the tray icon instead.
2. Press the hotkey to open the palette, type to search, arrow keys to
   navigate, **Enter** to paste the highlighted entry back into the previous
   window.
3. Use **Settings** for privacy and hotkey configuration. Run
   `nagori doctor` if something feels off; the desktop app surfaces the
   same capability matrix under **Settings → Advanced → Capabilities**.

The `nagori` CLI ships inside the desktop app bundle. Homebrew cask installs
link it onto your PATH automatically; for direct `.dmg` installs, open
**Settings → CLI → Command-line tool → Install nagori CLI** to symlink it into
`~/.local/bin`. See [`docs/cli.md`](./docs/cli.md#installation) for details.

CLI quick reference:

```sh
nagori list --limit 10            # recent clips
nagori search "kubectl"           # full-text search
nagori paste <id>                 # copy + auto-paste an entry
nagori capabilities               # what this OS build supports
```

Full CLI reference: [`docs/cli.md`](./docs/cli.md).

## FAQ

**Where is my data stored?**
`~/Library/Application Support/nagori` (macOS), `%LOCALAPPDATA%\nagori`
(Windows), or `$XDG_DATA_HOME/nagori` (Linux).

**Is the database encrypted?**
No. The DB file has restrictive filesystem permissions but is not encrypted at
rest. Use FileVault / BitLocker / LUKS for full-disk protection. SQLCipher
integration is on the roadmap.

**How does secret redaction work?**
The default mode stores matched secrets as `[REDACTED]` and re-derives hashes
and search tokens from the scrubbed form. Switch to `Store full` only if you
need the raw bytes — details in [`docs/privacy.md`](./docs/privacy.md).

**Do the AI features send my clipboard to the cloud?**
No. The macOS-only AI actions and semantic search are opt-in (off by default)
and run Apple's on-device models locally — no clipboard text is sent to a
remote API, and the Private Cloud Compute path is not used. The models and
language packs themselves are downloaded and managed by macOS. They need
macOS 26+; the text-generation actions additionally require Apple Silicon
with Apple Intelligence enabled, while Translate and semantic search depend
on their own OS-downloaded language packs and embedding assets. See
[`docs/privacy.md`](./docs/privacy.md) and
[`docs/platforms.md`](./docs/platforms.md) for the full contract and
requirements.

**Windows SmartScreen warns me on first launch.**
The NSIS installer is not yet Authenticode-signed, so every fresh download
trips the warning until the certificate is in place. Choose
**More info → Run anyway** to proceed.

**Auto-paste does not work on Linux.**
Install `wtype` and confirm the compositor exposes
`zwp_virtual_keyboard_manager_v1` (run `wtype test` while a text field has
focus). Troubleshooting steps live in [`docs/platforms.md`](./docs/platforms.md).

**The global hotkey does not register on Linux.**
`tauri-plugin-global-shortcut` is X11-only upstream. On pure Wayland sessions
the registration fails and the Settings page prompts for a different binding —
use the tray icon to toggle the palette until upstream support lands.

## Documentation

- [`ARCHITECTURE.md`](./ARCHITECTURE.md) — workspace layout and runtime topology.
- [`docs/platforms.md`](./docs/platforms.md) — compatibility matrix and per-OS notes.
- [`docs/privacy.md`](./docs/privacy.md) — privacy model, redaction, denylist.
- [`docs/cli.md`](./docs/cli.md) — CLI reference.
- [`docs/ipc.md`](./docs/ipc.md) — IPC envelope and transport.

## License

Licensed under the [MIT License](./LICENSE).

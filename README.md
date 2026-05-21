# nagori

Local-first clipboard history for macOS, Windows, and Linux Wayland.

## Features

- Persistent clipboard history (text, images, file lists) stored locally in SQLite.
- Full-text search and pinning from a desktop palette or the `nagori` CLI.
- Built-in secret classifier that redacts API keys, JWTs, AWS / GitHub tokens,
  PEM blocks, credit-card numbers, and OTPs before they hit disk.
- User regex denylist for project-specific patterns.
- Auto-paste back into the previously focused window (Cmd/Ctrl+V synthesis).
- Quick actions on a selected entry: summarise, format JSON, extract tasks,
  redact secrets — all computed locally without any network calls.
- URL preview shows host on its own row with a punycode badge when the
  displayed Unicode host differs from its ASCII form; press **Enter** in
  the expanded preview to open the URL in the default browser after a
  confirm dialog (Public entries only, `https` / `http` only).
- Image preview uses a daemon-cached 512px thumbnail on row navigation so
  the palette stays responsive on multi-megabyte screenshots; the
  expanded preview switches to the original payload for click-to-zoom.
- Bundled per-OS release artefacts with an in-app update-availability probe.

## Install

| Platform               | Bundle                                                       |
| ---------------------- | ------------------------------------------------------------ |
| macOS (arm64 / x86_64) | `.app` / `.dmg` from [GitHub Releases](https://github.com/mhiro2/nagori/releases) |
| Windows (x86_64)       | Unsigned NSIS installer (SmartScreen warns on first launch)  |
| Linux Wayland (x86_64) | `.deb` and `AppImage`                                        |

Linux additionally requires the `wtype` binary on `$PATH` for auto-paste and a
Wayland compositor that exposes `wlr_data_control` or `ext_data_control` (sway,
Hyprland, KDE Plasma 5.27+, …). See [`docs/platforms.md`](./docs/platforms.md)
for the full compatibility matrix and troubleshooting.

## Usage

1. Launch the desktop app — it starts the background daemon and registers a
   global hotkey (default `Ctrl+Shift+V`, `Cmd+Shift+V` on macOS). On Linux
   Wayland the upstream global-shortcut plugin is X11-only, so the registration
   fails and you toggle the palette from the tray icon instead.
2. Press the hotkey to open the palette, type to search, arrow keys to
   navigate, **Enter** to paste the highlighted entry back into the previous
   window.
3. Use **Settings** for privacy and hotkey configuration. Run
   `nagori doctor` if something feels off (the desktop app's
   **Settings → Advanced → Diagnostics** runs the same probe).

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

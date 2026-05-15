# nagori

Local-first clipboard history for macOS, Windows, and Linux Wayland.

## Platform support

Nagori is **macOS-first**. Windows and Linux Wayland are
**experimental**: the desktop app and CLI daemon build and run on each
target, and a CLI clipboard round-trip (capture → search → copy back)
is covered by per-OS smoke tests: macOS and Windows run on PRs that
touch relevant paths (and on every push to `main`), and Linux Wayland
runs on a nightly schedule. Desktop UI flows, auto-paste, and
global-shortcut registration are not yet exercised by automated tests
off macOS, and several integrations remain incomplete (Windows release
bundles, image capture and copy-back outside macOS, update checks
outside macOS, GNOME Wayland, X11).

| Platform               | Desktop app  | CLI daemon   | Capture       | Copy back   | Auto-paste        | Release bundle                       |
| ---------------------- | ------------ | ------------ | ------------- | ----------- | ----------------- | ------------------------------------ |
| macOS (arm64 / x86_64) | Supported    | Supported    | Supported     | Supported   | Supported         | Yes (`.app`, update notifications)   |
| Windows (x86_64)       | Experimental | Experimental | Text + files  | Text only   | Supported         | None — build from source             |
| Linux Wayland (x86_64) | Experimental | Experimental | Text only     | Text only   | Requires `wtype`  | Yes (tarball, no update notifications) |
| Linux X11              | Unsupported  | Unsupported  | Unsupported   | Unsupported | Unsupported       | n/a                                  |

macOS-only capabilities: secure-input detection, sleep/wake
`changeCount` resynchronisation, image clipboard capture and writes
(Windows and Linux fall back to text — and on Windows, file lists —
and return `Unsupported` when copying an image entry back), and the
bundled update-check probe. The Tauri updater plugin is registered on
every OS, but the runtime gates it to macOS and the MVP surface is
read-only: users still upgrade by following the GitHub release link
rather than an in-app `download_and_install` flow.

### Linux requirements

Linux support targets Wayland compositors that expose
`wlr_data_control` or `ext_data_control` — for example sway, other
wlroots-based compositors, KDE Plasma 5.27+, Hyprland, and river.

Known limitations:

- X11 sessions are not supported.
- GNOME Wayland exposes neither `wlr_data_control` nor
  `ext_data_control` and is not supported today.
- Auto-paste requires the `wtype` binary on `$PATH`.
- Global-shortcut registration is X11-only upstream; pure Wayland
  sessions cannot bind hotkeys and the failure is surfaced in the UI.
- `WindowBehavior::frontmost_app()` returns `None` because Wayland has
  no portable API to identify the frontmost client.

### Windows notes

Windows builds target x86_64 and use Win32 clipboard APIs
(`GetClipboardSequenceNumber` polling, `arboard` for text, `CF_HDROP`
for file lists) with `SendInput` Ctrl+V auto-paste and Tauri-managed
global shortcuts. Installer artifacts are not yet produced by the
release workflow — build from source until a signed Windows bundle
ships.

## Documentation

- [`ARCHITECTURE.md`](./ARCHITECTURE.md) — high-level overview of the
  workspace and runtime topology.
- [`docs/cli.md`](./docs/cli.md) — CLI usage reference.
- [`docs/ipc.md`](./docs/ipc.md) — IPC envelope and transport.

## License

Licensed under the [MIT License](./LICENSE).

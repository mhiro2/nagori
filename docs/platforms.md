# Platforms

Compatibility matrix, per-OS requirements, and troubleshooting for
nagori. The runtime answer for the build you have installed is
`nagori capabilities` (add `--json` for machine-readable output); the
desktop shell mirrors the same matrix under
**Settings → Advanced → Platform capabilities**.

## Compatibility matrix

| Platform               | Desktop app | CLI daemon | Capture   | Copy back | Auto-paste        | Release bundle                                |
| ---------------------- | ----------- | ---------- | --------- | --------- | ----------------- | --------------------------------------------- |
| macOS (arm64 / x86_64) | Supported   | Supported  | Supported | Supported | Supported         | Yes (unsigned `.app` / `.dmg`, in-app update probe) |
| Windows (x86_64)       | Supported   | Supported  | Supported | Supported | Supported         | Yes (unsigned NSIS, in-app update probe)      |
| Linux Wayland (x86_64) | Supported   | Supported  | Supported | Supported | Supported (note*) | Yes (`deb` + `AppImage`, in-app update probe) |
| Linux X11              | Unsupported | Unsupported | —        | —         | —                 | n/a                                           |

*Linux auto-paste depends on the `wtype` binary being on `$PATH` and on
the compositor advertising `zwp_virtual_keyboard_manager_v1`. See
[Linux requirements](#linux-requirements) below.

The clipboard pipeline (capture of text / image / file-list, copy-back,
and `nagori paste` auto-paste) is covered by per-OS CI smoke tests:
macOS and Windows run on PRs that touch relevant paths (and on every
push to `main`), and Linux Wayland runs on a nightly schedule. The
desktop shell's palette UI flow is not yet exercised by an automated
WebView test — that work is tracked separately for tauri-driver.

macOS-only capabilities: secure-input detection, sleep/wake
`changeCount` resynchronisation, and Quick Look preview (Cmd+Y in
the palette, dispatched through `/usr/bin/qlmanage -p`; restricted to
Public entries). The Tauri updater plugin is registered
on every supported OS and the release workflow publishes a signed
`latest.json` feed for macOS, Windows, and Linux, so the in-app
availability probe runs everywhere. The current MVP surface is
read-only — users upgrade by following the GitHub release link — and
the wording differs by install medium: bundles that the updater could
swap in place (`.app` / `.dmg`, NSIS, `AppImage`) show "View release",
while `deb` installs show "Download manually" to reflect that the
GitHub artefact has to be re-installed by hand. The macOS `.app` /
`.dmg` are not codesigned (Gatekeeper warns on first launch) and the
NSIS bundle is not Authenticode-signed (SmartScreen warns on first
launch).

## Linux requirements

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

### Linux troubleshooting

Run these checks before filing an issue — most "capture doesn't work"
reports trace back to one of them.

- **Verify the session is Wayland.** Nagori needs to reach a Wayland
  compositor, so `$WAYLAND_DISPLAY` must be non-empty and refer to a
  live socket. `$XDG_SESSION_TYPE` is usually `wayland` on graphical
  logins but can be empty or unexpected under nested / headless
  compositors — treat `$WAYLAND_DISPLAY` as the authoritative signal.
  If you are stuck on X11, switch to a Wayland session at login.
- **Verify the compositor exposes a `data_control` manager.** Inspect
  the compositor's advertised globals with `wayland-info` (from the
  `wayland-utils` package) and look for either
  `zwlr_data_control_manager_v1` (wlroots) or
  `ext_data_control_manager_v1` (ext). If neither is listed, the
  compositor lacks the required protocol and Nagori cannot capture
  the clipboard. Known-good: sway, Hyprland, river, other
  wlroots-based compositors, KDE Plasma 5.27+. Known-bad: GNOME
  Wayland. When the desktop shell hits this case at launch it opens
  a small fallback window with the same annotated error the CLI
  prints and a link back here, so launching from a desktop file (no
  visible stderr) still surfaces the cause.
- **Verify `wtype` is installed.** `wtype --help` should print usage.
  On Debian/Ubuntu install with `apt install wtype`; on Arch
  `pacman -S wtype`. Nagori's doctor probe uses this same call to
  decide whether to mark Accessibility as Granted.
- **Verify auto-paste actually works.** `wtype` additionally needs
  the compositor to expose `zwp_virtual_keyboard_manager_v1` (the
  global from which `zwp_virtual_keyboard_v1` objects are created).
  Confirm by focusing a text field and running `wtype test` — if it
  prints `Compositor does not support the virtual keyboard
  protocol`, auto-paste will fail at runtime even though the binary
  is installed. Without `wtype`, or when the virtual-keyboard
  protocol is missing, Enter in the palette copies the entry to the
  clipboard but does not paste into the previous window.
- **In-app global shortcuts don't register.**
  `tauri-plugin-global-shortcut` is X11-only upstream; on pure
  Wayland sessions `register_primary_hotkey` fails and the desktop
  shell emits `nagori://hotkey_register_failed` so the Settings page
  can prompt the user to pick a different binding (no
  silent-disable, no retry loop). There is no in-app workaround
  today — use the tray icon to toggle the palette. (Compositor-level
  keybindings such as sway/Hyprland bindings cannot reach the
  running Nagori process either, since Nagori does not yet expose a
  CLI/IPC entry point for palette toggle.)

## Windows notes

Windows builds target x86_64 and use Win32 clipboard APIs:
`GetClipboardSequenceNumber` polling to detect changes, `arboard`
for text and `CF_DIBV5` / `CF_DIB` image reads, a hand-rolled
`DROPFILES` + `SetClipboardData(CF_HDROP)` writer for file lists,
`DragQueryFileW` for file-list reads, and a multi-format publisher
that batches `CF_UNICODETEXT` + `CF_HTML` + `Rich Text Format` +
`CF_DIBV5` (with a registered `"PNG"` companion) + `CF_HDROP` in a
single `OpenClipboard` / `EmptyClipboard` / N × `SetClipboardData`
transaction so Preserve copy-back keeps every stored representation
on the clipboard. Auto-paste is `SendInput` Ctrl+V and global
shortcuts use the Tauri global-shortcut plugin. `SendInput` needs no
Accessibility-style permission, so auto-paste works out of the box and
the desktop omits the Settings **Setup** tab entirely on Windows (there
is nothing to grant or install).

Supported environment:

- Windows 10 1809 or later, or Windows 11. The desktop shell embeds
  WebView2, which requires the Edge WebView2 runtime
  (preinstalled on every supported version).
- Standard user privileges. Nagori does not require Administrator
  rights and is not designed to run elevated.
- A normal interactive desktop session. Nagori does not run as a
  Windows service and does not currently observe the
  per-session clipboard from a different security context.

Known limitations:

- The release workflow ships an unsigned per-user NSIS installer.
  SmartScreen warns on first launch (`More info → Run anyway`).
  Authenticode signing is still pending; until it lands, every fresh
  download will trip the SmartScreen warning because the certificate
  is missing.
- Auto-paste cannot target an elevated foreground window from a
  non-elevated Nagori process: `SendInput` is blocked by UIPI when
  the target window runs at a higher integrity level. Run Nagori at
  the same level as the apps you paste into.

## macOS notes

Supported environment:

- macOS 26 (Tahoe) or later on Apple Silicon and Intel. The bundle
  declares `LSMinimumSystemVersion = 26.0` via
  `bundle.macOS.minimumSystemVersion` in `tauri.conf.json`, so the
  installer refuses to launch on earlier releases. The 0.0.x line is
  validated only against Tahoe — auto-paste in particular routes its
  ⌘V synthesis around `TSMGetInputSourceProperty`, which trips
  `dispatch_assert_queue(main)` and aborts from non-main threads on
  macOS 26+, so the workaround is required there but unnecessary on
  earlier releases.

The desktop shell runs as an `NSApplicationActivationPolicyAccessory`
application: the menu-bar tray is the primary entry point and no Dock
icon is shown, matching the per-window `skipTaskbar: true` intent of
the palette window. The Dock icon is controlled per-process by NSApp's
activation policy on macOS (not per-window), so without `Accessory`
the icon would flicker in and out of the Dock every time the palette
is shown or hidden, and the app would appear in Cmd+Tab. The flip is
applied only after tray installation succeeds — if the tray cannot be
installed, the default `Regular` policy stays in place so the user
still has the Dock and Cmd+Tab as a way back to the (hidden) main
window. The startup fallback window (shown when `AppState::try_new`
fails) also keeps the default `Regular` policy so the error window
remains reachable even though tray installation is skipped in that
branch.

Known limitations:

- The release workflow ships unsigned `.app` / `.dmg` bundles.
  Gatekeeper warns on first launch — right-click → **Open**, or run
  `xattr -d com.apple.quarantine /Applications/Nagori.app` to clear
  the quarantine attribute. Codesigning and notarization are not on
  the roadmap.

### On-device AI features (opt-in)

The model-backed AI actions (Summarize, Translate, Rewrite, Format
Markdown, Extract tasks, Explain code) and semantic search are
macOS-only and off by default. They are opt-in and experimental on the
`0.0.x` canary line, and split across three Apple frameworks with
**independent availability** — one being unavailable does not disable
the others:

- **Text generation** (Summarize, Rewrite, Format Markdown, Extract
  tasks, Explain code) runs on Foundation Models / Apple Intelligence.
  It requires **Apple Silicon (M1 or later)** — Intel Macs report
  `deviceNotEligible` — and **Apple Intelligence enabled** in System
  Settings, which Nagori cannot turn on programmatically. Until it is
  on, **Settings → AI** shows the status as unavailable and `nagori
  doctor` reports `appleIntelligenceNotEnabled`. The on-device model
  needs **~7 GB of free space** and supports a fixed set of locales
  (English, Japanese, Korean, Chinese, and major European languages),
  with the device and Siri language required to match a supported
  language; Japanese support landed in macOS 26.1, and mainland China
  has additional restrictions.
- **Translate** runs on the Translation framework, **independent of
  Apple Intelligence**. It needs the relevant per-language pack
  installed (downloaded and managed by macOS on first use); a missing
  pair surfaces as an asset-missing error with a download remediation.
- **Semantic search** runs on `NLContextualEmbedding`, also
  **independent of Apple Intelligence**, and gates on its own
  `semantic_index_enabled` toggle plus the embedding asset (downloaded
  by macOS on first use). The embedder is pinned to one language/model
  so every stored vector shares a comparable space.

All model and language assets are downloaded and managed by macOS, not
bundled with Nagori. On macOS, when a capability is unavailable (e.g.
Apple Intelligence not yet enabled) its AI action buttons are disabled
with a tooltip explaining why, and `nagori doctor` reports
text-generation, translation, and embedding availability separately. On
Windows and Linux — where no AI engine is wired — the AI surfaces are
hidden outright rather than shown disabled: the **Settings → AI** tab and
the action menu's AI actions do not appear at all. (The desktop gates this
on the `ai_actions` platform capability, so a future cross-OS backend
would restore the surfaces automatically.) See
[`docs/privacy.md`](./privacy.md#ai-actions-and-on-device-models-macos)
for the privacy contract (on-device inference, no Private Cloud
Compute, OS-managed model downloads).

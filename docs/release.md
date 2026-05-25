# Release process

This document captures the workflow `.github/workflows/release.yaml`
expects to run against and the one-time setup needed for a release
with signed updater artifacts. The user-facing surface — install
media, platform support, the in-app updater — is described in
[`docs/platforms.md`](./platforms.md) and in
[ARCHITECTURE.md §16 → Updater](../ARCHITECTURE.md#16-desktop-shell-integration).

## What `release.yaml` produces

When a `v*` tag is pushed, `release.yaml` builds one bundle row per
matrix entry and uploads each artifact (plus an entry in the shared
`latest.json` updater manifest) to a GitHub draft release. After every
row finishes, the `publish` job flips the draft to a public release.
`0.0.x` tags are marked as GitHub pre-releases — the workflow inspects
the tag name and passes `prerelease: true` to `tauri-action` while
`0.0.*` is in effect, then switches back to `false` once `>= 0.1.0`
tags ship.

| Target                          | Bundles        | Probe verdict shown in Settings → Advanced     |
| ------------------------------- | -------------- | ---------------------------------------------- |
| `aarch64-apple-darwin`          | `app`, `dmg`   | "View release" — bundle is in-place swappable  |
| `x86_64-apple-darwin`           | `app`, `dmg`   | "View release" — bundle is in-place swappable  |
| `x86_64-pc-windows-msvc`        | `nsis`         | "View release" — bundle is in-place swappable  |
| `x86_64-unknown-linux-gnu`      | `deb`, `appimage` | `AppImage`: "View release". `deb`: "Download manually" — needs a manual `dpkg` install |

The desktop shell calls `updater.check()` for the version comparison
but does **not** call `update.download_and_install()` — the MVP
surface stays read-only and links to the GitHub release. The wording
toggle above is driven by `commands::in_place_update_supported()`,
which delegates to `tauri::utils::platform::bundle_type()` so the
labels line up with the bundle the updater would have replaced.

The matrix runs with `max-parallel: 1` because `tauri-action` reads
the draft release's existing `latest.json`, appends the current row's
platform entry, deletes the old asset, and re-uploads — parallel rows
would race on that asset and only the last writer's platform would
survive.

`bundle.createUpdaterArtifacts: true` in `apps/desktop/src-tauri/tauri.conf.json`
makes Tauri emit the matching `*.sig` companion for every bundle, and
`tauri-action`'s `includeUpdaterJson: true` rolls those companions into
the signed `latest.json` feed that the in-app updater reads from
`https://github.com/mhiro2/nagori/releases/latest/download/latest.json`.

## One-time setup

### 1. Generate the updater signing keypair

The updater plugin verifies every downloaded bundle against the public
key embedded in `tauri.conf.json`. Generate the keypair once per
release line:

```sh
pnpm --dir apps/desktop exec tauri signer generate -w ~/.tauri/nagori.key
```

The command prints both halves. Treat the private half as a secret —
anyone who has it can publish a payload the updater will trust.

### 2. Commit the public key

Paste the printed public key into
`apps/desktop/src-tauri/tauri.conf.json` under
`plugins.updater.pubkey`. The `Verify updater pubkey is configured`
step in `release.yaml` fails the tag build if this field is empty or a
placeholder, so the project will refuse to ship an unverifiable bundle.

### 3. Register the GitHub Actions secrets

Add the secrets the workflow expects to **Settings → Secrets and
variables → Actions**:

| Secret                                 | Purpose                                     | Required for                |
| -------------------------------------- | ------------------------------------------- | --------------------------- |
| `TAURI_SIGNING_PRIVATE_KEY`            | Private half of the updater keypair         | Every target (mandatory)    |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`   | Password used when generating the keypair   | Every target (mandatory)    |

macOS code signing & notarization is intentionally not wired up — the
`.app` / `.dmg` ship unsigned and Gatekeeper warns on first launch.

Windows Authenticode signing is not wired up yet — until an EV cert
lands, SmartScreen warns on first launch.

## Local builds

`createUpdaterArtifacts: true` means a plain
`pnpm --dir apps/desktop tauri build` also tries to emit signed
updater sidecars for every bundle in the build matrix (the macOS
`.app`, the Linux `AppImage`, the Windows NSIS — all of them — and
will abort if it can't reach a signing key. Local contributors have
two options:

- Set `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
  in the local environment (use a throwaway dev key — the public half
  in `tauri.conf.json` already pins the production verification key,
  so a local dev build will never be accepted by an end-user updater).
- Or override the config inline:
  `pnpm --dir apps/desktop tauri build --config '{ "bundle": { "createUpdaterArtifacts": false } }'`
  to skip updater sidecars entirely for one-off iteration. Restricting
  the bundle list with `--bundles app` still tries to sign the
  surviving target, so the override is needed for an unsigned build.

## Cutting a release

1. Bump `apps/desktop/src-tauri/Cargo.toml` and the root workspace
   versions and land the commit. The `Verify Cargo version matches
   release tag` step checks the `nagori-desktop` crate version against
   the tag name and fails fast on a mismatch.
2. Tag the merge commit `vX.Y.Z` and push the tag.
3. `release.yaml` builds each row, uploads the bundle plus its
   `latest.json` sidecar to a draft GitHub release, and the `publish`
   job promotes the draft to a published release once every matrix row
   succeeds.
4. The next time any installed copy of nagori starts with
   **Settings → Advanced → Updates → Check for updates automatically**
   on (or the user clicks **Settings → Advanced → Check for updates
   now** explicitly — that button bypasses the toggle), the updater
   probe will read `latest.json` and surface the new version.
   Bundles the updater could swap in place (`.app` / `.dmg`, NSIS,
   `AppImage`) link to the release with "View release" copy; `deb`
   installs show "Download manually" instead. The desktop shell does
   not currently call `update.download_and_install()` — every install
   medium points the user at the GitHub release page for the actual
   upgrade.

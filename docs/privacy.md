# Privacy and security

Nagori is local-first: capture, search, redaction, and Quick actions
all run in your process. The daemon's only background network call
is the update-availability probe against GitHub Releases, and it
runs only while **Settings → Advanced → Updates → Check for updates
automatically** is on (default: on). Turn that toggle off to keep
the daemon fully offline.

## Data at rest

- The SQLite database, search index, and per-launch IPC token live
  under `$XDG_DATA_HOME/nagori` (Linux), `~/Library/Application
  Support/nagori` (macOS), or `%LOCALAPPDATA%\nagori` (Windows).
- The DB file itself is forced to `0600` and its parent directory
  to `0700` on macOS / Linux; on Windows the path inherits the
  default NTFS DACL from `%LOCALAPPDATA%`, which already restricts
  read access to your account.
- **The DB is not encrypted at rest.** Permission bits keep other
  local users out, but they do not defend against anything running
  as your user (backups, cloud-sync clients, malware). If your home
  directory is on a synced folder, exclude `nagori/` or store the
  data directory outside it. Prefer FileVault / BitLocker / LUKS
  for full-disk protection.
- SQLCipher / OS keychain integration is on the roadmap but **not
  implemented**. The blockers (dependency size, schema migrations
  against an encrypted DB, and a recovery story when the key
  store rotates) are tracked in
  [`security-encryption-at-rest.md`](./security-encryption-at-rest.md).

## Secret redaction

- A built-in classifier flags API keys, JWTs, PEM private-key
  blocks (BEGIN-only is enough), AWS access keys, GitHub tokens,
  Luhn-checked credit-card runs, OTP-style 6–8 digit bodies, and
  the source app's bundle id against the password-manager list.
- The default **secret handling** is `Store redacted`: matched
  clips land in SQLite with the secret replaced by `[REDACTED]`,
  and the content hash, normalized text, and search tokens are all
  recomputed from the scrubbed form. Switching to `Store full`
  requires an explicit in-app confirmation because the durable
  copy then keeps the raw bytes.
- `Store redacted` rewrites *new* captures only. Pre-existing
  rows, the SQLite freelist, and any backup still carry the
  original bytes — delete the affected rows, run
  `PRAGMA wal_checkpoint(TRUNCATE)` (or stop the daemon) so the
  `nagori.sqlite-wal` sidecar gets folded back into the main file,
  then `VACUUM` if you need a clean DB. The same checkpoint step
  is what your backup tooling needs before snapshotting the data
  directory, otherwise the copy silently loses the last-N captures
  that haven't been written through yet.

## App denylist

The privacy panel exposes two controls under
**Settings → Privacy → App denylist**:

### Block password managers (preset, default ON)

A bundled list of exact app identifiers. Captures whose source app
matches any entry are classified as `Blocked` and never written to
history. The toggle is on by default and is recommended unless you
actively need to copy from a password manager via the clipboard.

The preset is fixed (not user-editable). Current entries:

- macOS bundle IDs:
  - `com.1password.1password` — 1Password 8 / Setapp
  - `com.agilebits.onepassword7` — 1Password 7
  - `com.agilebits.onepassword4` — 1Password (legacy)
  - `com.bitwarden.desktop` — Bitwarden desktop
  - `org.keepassxc.keepassxc` — KeePassXC
  - `com.apple.Passwords` — Apple Passwords
- Windows executable basename (case-insensitive, no `.exe`):
  - `1password`, `bitwarden`, `keepassxc`

The Windows side matches the executable basename so MSIX / `Program
Files (x86)` path variants resolve to the same rule without
per-install normalisation. Linux desktop sessions that cannot
expose the frontmost app (Wayland on most compositors) disable the
denylist UI entirely and surface a banner — per-app blocking would
silently match nothing there.

Tracking the full universe of password managers would be a
moving-target maintenance burden, so the preset only covers the
clients we can confidently pin with stable identifiers. For
anything outside the list — Dashlane, LastPass desktop, Enpass,
1Password browser extensions running inside a host browser, an
internal tool you want to exclude — use **Custom patterns** below.

### Custom patterns (free-text substring, default empty)

One pattern per line. A capture is dropped when its source-app
name, bundle ID, or executable path contains the line as a
case-insensitive substring. Patterns are independent from the
preset — disabling the toggle does not remove user-entered
patterns, and vice versa.

Custom patterns are the right place for:

- Password managers not in the bundled preset (Dashlane, LastPass,
  Enpass, …).
- Internal / company tools you do not want clipped to history.
- Browser-extension password managers, by matching the host
  browser's bundle ID when the extension is open in a dedicated
  profile.

## User regex denylist

The privacy panel accepts user-defined patterns under
**Settings → Privacy → Regex denylist**. Anything that matches is
classified as `Blocked` and refused storage entirely.

To keep a hostile or accidentally pathological rule from wedging the
daemon, each entry is gated by the limits enforced in
`nagori-core::policy::compile_user_regex`:

- **256 bytes** per pattern (`MAX_USER_REGEX_LEN`).
- **3 levels** of unescaped parenthesis nesting
  (`MAX_USER_REGEX_NESTING`) — `\(` and `\)` don't count, so
  literal brackets are fine.
- **256 KiB** compiled NFA budget and **1 MiB** lazy-DFA cache per
  pattern (`size_limit` / `dfa_size_limit` on `RegexBuilder`).

If a rule trips a limit, the Settings UI surfaces the offending
line with a fix hint ("split across multiple lines", "flatten the
groups", …) instead of a generic save failure. Split complex
intents into multiple lines rather than nesting groups — the
denylist is an `OR` of every line.

## Preview thumbnails and external URL open

- Image entries get a 512px cached thumbnail under
  `entry_thumbnails` so the preview pane stays responsive on
  multi-megabyte screenshots. Generation is gated by sensitivity —
  the daemon refuses to derive a thumbnail for `Private`, `Secret`,
  or `Blocked` entries, so image bytes from those entries never
  leak into the cached preview surface. The table is a regenerable
  cache: an LRU sweep bounded by `max_thumbnail_total_bytes`
  (default 64 MiB) evicts cold rows, and `ON DELETE CASCADE`
  removes the thumbnail whenever the source entry is deleted.
- The "open URL in browser" action from the expanded preview is
  also gated to `Public` entries with an `https` / `http` scheme,
  and the desktop pops a confirm dialog that shows the resolved host
  (with a punycode badge when the displayed Unicode host differs
  from its ASCII form) before invoking the OS shell handler. Other
  schemes — `file://`, `javascript:`, `data:`, custom protocol
  handlers — are refused without a prompt.

## Quick actions and network

- Quick actions (Summarize, Format JSON, Extract tasks, Redact
  secrets) run entirely on-device against the rule-based runner —
  no remote provider is dispatched.
- The runner re-applies the secret scrubber to its input as a
  defence-in-depth pass (`AiInputPolicy::require_redaction`) so a
  result block can never contain a token the classifier already
  flagged on the source entry.
- The daemon's only background outbound network use is the
  update-availability probe against GitHub Releases. **Settings →
  Advanced → Updates → Check for updates automatically** controls
  it; turning it off keeps the daemon fully offline (the same
  toggle gates both the desktop startup probe and the
  `latest_version` lookup that `nagori doctor` shows). The manual
  **Check for updates now** button bypasses this toggle by design —
  it is an explicit user action and always reaches the network when
  pressed.
- `nagori doctor` prints `auto_update_check` so operators can
  confirm at a glance whether anything is allowed to reach the
  network.
- Clipboard bodies are never written to logs — only metadata
  (declared MIME, detected MIME, byte counts, sensitivity verdict)
  shows up in tracing output.

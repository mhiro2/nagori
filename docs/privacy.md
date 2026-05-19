# Privacy and security

Nagori is local-first: capture, search, redaction, and Quick actions
all run in your process and the daemon never reaches a network on its
own.

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

## Quick actions and network

- Quick actions (Summarize, Format JSON, Extract tasks, Redact
  secrets) run entirely on-device against the rule-based runner —
  no remote provider is dispatched, regardless of `Local-only mode`.
- The runner re-applies the secret scrubber to its input as a
  defence-in-depth pass (`AiInputPolicy::require_redaction`) so a
  result block can never contain a token the classifier already
  flagged on the source entry.
- The daemon's only outbound network use is the periodic
  update-availability probe against GitHub Releases. `Privacy →
  Local-only mode` toggles that probe off and the in-app update
  banner stays silent.
- Clipboard bodies are never written to logs — only metadata
  (declared MIME, detected MIME, byte counts, sensitivity verdict)
  shows up in tracing output.

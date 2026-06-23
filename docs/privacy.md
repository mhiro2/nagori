# Privacy and security

Nagori is local-first: capture, search, redaction, and Quick actions
all run in your process. The daemon's only background network call
is the update-availability probe against GitHub Releases, and it
runs only while **Settings → Advanced → Updates → Check for updates
automatically** is on (default: on). Turn that toggle off to keep
the daemon fully offline — with one caveat: the opt-in, macOS-only AI
and semantic-search features run their inference on-device but depend
on OS-managed Apple model downloads that the daemon cannot suppress.
Both are off by default; see [AI actions and on-device
models](#ai-actions-and-on-device-models-macos) for the full
contract.

## Data at rest

- The SQLite database, search index, and per-launch IPC token live
  under `$XDG_DATA_HOME/nagori` (Linux), `~/Library/Application
  Support/nagori` (macOS), or `%LOCALAPPDATA%\nagori` (Windows).
- The DB file itself is forced to `0600` and its parent directory
  to `0700` on macOS / Linux; on Windows the path inherits the
  default NTFS DACL from `%LOCALAPPDATA%`, which already restricts
  read access to your account.
- **The DB is not encrypted at rest, and app-level encryption is
  intentionally deferred.** Permission bits keep other local users out,
  but they do not defend against anything running as your user (backups,
  cloud-sync clients, malware). **Full-disk encryption (FileVault /
  BitLocker / LUKS) is the recommended at-rest baseline.**
- If your data directory is inside a synced folder, the plaintext history
  is copied to the cloud. Move it out (or exclude `nagori/` from syncing).
  Nagori detects the common sync folders (iCloud Drive, Dropbox, OneDrive,
  Google Drive, …) and warns you in **Settings → Privacy**; `nagori doctor`
  also reports it (`data_dir_sync_warning`) when it inspects the database
  directly (the local / `--db` arm).
- SQLCipher / OS-keystore encryption is **not implemented and is deferred
  past 1.0 by design**, not a checkbox left unticked. An OS-keystore key
  would protect a stolen powered-off disk, a plaintext backup, or a sync
  copy — but not a same-user attacker (the key unlocks for your login and
  a running Nagori already exposes the decrypted data), so it adds little
  over full-disk encryption. Encrypting only some columns is worse: the
  search index would still hold your text in the clear. The full rationale
  and the conditions that would reopen the decision are in
  [`ARCHITECTURE.md` §19](../ARCHITECTURE.md#19-security-notes).

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
- **Block all sensitive captures** refuses storage for both
  `Private` and `Secret` clips. This is stricter than `Store
  redacted` because nothing is inserted at all.

## Delete and purge semantics

Nagori separates "hide this entry from history" from "physically reclaim the
row" so the interactive Delete path can stay responsive without pretending
that every byte disappeared immediately:

- Normal **Delete** tombstones `Public`, `Unknown`, and `Private` rows. They
  stop appearing in list/search/copy surfaces immediately, and search rows are
  removed, but the entry row and representation payloads remain until
  maintenance purges tombstones.
- `Secret` rows are always hard-deleted immediately. A tombstone would keep raw
  or redacted secret payloads, representation blobs, thumbnails, and embeddings
  on disk until the next maintenance sweep.
- **Settings → Privacy → Delete entries permanently** makes normal Delete use
  the immediate hard-delete path for every sensitivity level.
- **Settings → Privacy → Purge deleted entries now** physically removes all
  tombstoned rows on demand. The maintenance loop runs the same purge
  periodically.
- **Clear history**, clear-on-quit, retention-by-age, retention-by-count, and
  total-byte-budget eviction are hard-delete paths. They cascade through
  representations, FTS/ngram rows, thumbnails, and semantic embeddings and then
  truncate the WAL when rows were removed.

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

## Owner exclusion markers (all platforms, always on)

Independently of the denylist, nagori honours the markers a clipboard owner
sets to declare "do not record this in history". On macOS these are the
[nspasteboard.org](https://nspasteboard.org) convention types:

- `org.nspasteboard.ConcealedType` — a secret (set by password managers
  such as 1Password / KeePassXC when you copy a credential).
- `org.nspasteboard.TransientType` — a throwaway value not meant to
  outlive the current paste.

When either marker is present, the capture is skipped: on macOS the adapter
probes for the marker **before reading the clipboard body**, so a marked
secret is normally never read at all, and **re-checks after the read** so a
marker that races in mid-publish still discards the just-read body without
storing it. The skip is audited as `capture_skipped` (`concealed_marker` /
`transient_marker`); when both markers are present, the concealed one wins.

This is robust for every normal single-publish copy. The one residual gap is
a multi-publish torn race — a different app's clipboard write flickering an
unmarked → marked → unmarked sequence inside the same sub-millisecond read —
which is the same torn-snapshot tradeoff every capture makes and is not a
realistic accident or attack vector.

This is **always on and has no setting**. The marker is the source app's
explicit non-persistence contract — exposing a toggle would only let nagori
break that contract — which is the same reasoning behind the unconditional
secure-text-field guard. The denylist and secret classifier still run as
independent additional layers for apps that do not set a marker.

Other platforms have analogous conventions, and nagori honours them on the
same always-on skip path:

- **Windows** — the `Clipboard Viewer Ignore` format (the long-standing
  convention password managers like KeePass set) and Microsoft's
  `ExcludeClipboardContentFromMonitorProcessing` format.
- **Linux (Wayland)** — KDE's `x-kde-passwordManagerHint` offer, set by
  KeePassXC / KWallet and other Plasma-aware apps.

These are presence-only secret markers (there is no transient analogue), so
they are treated like `ConcealedType`: the marked clip is skipped without its
body being read. The skip is audited the same way (`capture_skipped` with
`concealed_marker`).

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
- **128 patterns** in total (`MAX_USER_REGEX_COUNT`) — each pattern
  runs against every capture, so the count is capped both when
  settings are saved and again when the classifier is built, so no
  list that bypassed validation can defeat the per-pattern limits in
  aggregate.

If a rule trips a limit, the Settings UI surfaces the offending
line with a fix hint ("split across multiple lines", "flatten the
groups", …) instead of a generic save failure. Split complex
intents into multiple lines rather than nesting groups — the
denylist is an `OR` of every line.

## Preview thumbnails and external URL open

- Image entries get a 512px cached thumbnail under
  `entry_thumbnails` so the preview pane stays responsive on
  multi-megabyte screenshots. File lists that carried an image
  render alongside the file URLs (e.g. a presentation copied from
  Finder) reuse the same thumbnail cache for that accompanying
  image. Generation is gated by sensitivity —
  the daemon refuses to derive a thumbnail for `Private`, `Secret`,
  or `Blocked` entries, so image bytes from those entries never
  leak into the cached preview surface. The table is a regenerable
  cache: an LRU sweep bounded by `max_thumbnail_total_bytes`
  (default 64 MiB) evicts cold rows, and `ON DELETE CASCADE`
  removes the thumbnail whenever the source entry is deleted.
- Quick Look (macOS Cmd+Y) materialises the previewed `Public`
  entry to a plaintext temp file under
  `std::env::temp_dir()/nagori-preview/<entry_id>.<ext>` so the OS
  preview generator can read it. That cache is scrubbed at every
  history-erasure point so a previewed body does not outlive its
  row: the whole directory is wiped at app launch, `delete` removes
  the entry's file, and **Clear history** / **clear-on-quit** purge
  the directory. The files are regenerated on demand on the next
  preview, so the scrub is lossless.
- The "open URL in browser" action from the expanded preview is
  also gated to `Public` entries with an `https` / `http` scheme,
  and the desktop pops a confirm dialog that shows the resolved host
  (with a punycode badge when the displayed Unicode host differs
  from its ASCII form) before invoking the OS shell handler. Other
  schemes — `file://`, `javascript:`, `data:`, custom protocol
  handlers — are refused without a prompt.

## Quick actions and network

- Quick actions (Format JSON, Extract tasks, Redact secrets,
  Summarize first sentence) run entirely on-device against the
  rule-based runner — they never touch a language model and no
  remote provider is dispatched.
- The runner re-applies the secret scrubber to its input as a
  defence-in-depth pass (`AiInputPolicy::require_redaction`) so a
  result block can never contain a token the classifier already
  flagged on the source entry. The same redaction pass guards the
  model-backed AI actions described below.
- The daemon's only Nagori-initiated background outbound network use
  is the update-availability probe against GitHub Releases. **Settings →
  Advanced → Updates → Check for updates automatically** controls
  it; turning it off keeps Nagori's own outbound traffic off (the same
  toggle gates both the desktop startup probe and the
  `latest_version` lookup that `nagori doctor` shows). With the AI and
  semantic toggles also off, that leaves the daemon fully offline; the
  opt-in AI features rely on OS-managed Apple downloads the daemon
  cannot suppress (see [AI actions and on-device
  models](#ai-actions-and-on-device-models-macos)). The manual
  **Check for updates now** button bypasses this toggle by design —
  it is an explicit user action and always reaches the network when
  pressed.
- `nagori doctor` prints `auto_update_check` so operators can
  confirm at a glance whether anything is allowed to reach the
  network.
- Clipboard bodies are never written to logs — only metadata
  (declared MIME, detected MIME, byte counts, sensitivity verdict)
  shows up in tracing output.

## AI actions and on-device models (macOS)

The model-backed AI actions (Summarize, Translate, Rewrite, Format
Markdown, Extract tasks, Explain code) and semantic search are
**macOS-only and opt-in**. They are wired to Apple's on-device
frameworks — Foundation Models / Apple Intelligence for text
generation, the Translation framework for Translate, and
`NLContextualEmbedding` for semantic search. Both switches default to
off: the AI master toggle (`ai.enabled`) and the separate semantic
index toggle (`ai.semantic_index_enabled`). On other platforms no AI
engine is wired, so the actions report a capability mismatch and the
quick actions above keep working unchanged.

### Inference is on-device; the path is not Private Cloud Compute

- All inference runs on-device through Apple's local frameworks. The
  Apple backend pins `AiInputPolicy::allow_remote = false`, so no
  clipboard text is sent to a remote inference API and there is no
  remote-provider fallback on this path.
- **Private Cloud Compute is not used.** The text path drives only the
  on-device `SystemLanguageModel.default`; if that model cannot serve a
  request it errors locally rather than offloading the prompt to
  Apple's server-side models.
- The input-policy pipeline (`require_redaction`, the secret/blocked
  sensitivity rules, and the ~3,500-token budget that refuses
  oversized input instead of letting the model silently truncate)
  runs **before** any model sees the text, exactly as it does for the
  rule-based quick actions.

### Prompts stay local; model assets are downloaded by the OS

- Your prompts and the clipboard text they carry stay on-device.
- The **models and language assets** are downloaded and managed by
  macOS, not by Nagori: Apple Intelligence downloads its text model,
  the Translation framework downloads per-language packs the first
  time a pair is used, and `NLContextualEmbedding` downloads its
  embedding assets on first use. Those downloads are OS-driven, reach
  Apple's servers, and are outside Nagori's control.
- The **Settings → Advanced → Updates → Check for updates
  automatically** toggle gates only Nagori's own GitHub release probe.
  It does **not** suppress these OS-level model / asset downloads or
  any other Apple Intelligence background traffic — the AI and
  Translate features inherently rely on OS services that can talk to
  Apple, and that cannot be fully disabled from inside Nagori. Leave
  the AI toggles off if you need the daemon to stay fully offline.

### Translation framework telemetry

Apple may collect **usage and performance metrics** for the
Translation framework — bundle identifier, language pair, and similar
operational signals, **not** the text being translated. This is Apple
platform behaviour that Nagori cannot opt out of on the user's behalf;
it applies whenever the Translate action runs.

### Semantic index data at rest

When the semantic index toggle is on, embeddings are computed
on-device and the resulting float32 vectors are stored locally in the
same SQLite database as the rest of your history (`entry_embeddings`,
keyed by `entry_id`). They inherit the same at-rest posture as
everything else in the DB — restrictive filesystem permissions,
**not** encrypted at rest — so the [Data at rest](#data-at-rest)
guidance applies to them too.

The index is sensitivity-aware about *what* it embeds. **Secret**
entries are never embedded — even under `StoreFull`, a secret's raw
body is never handed to the embedding model — so no Secret-derived
vector is ever produced or stored. **Private** entries are embedded
only after the same redaction that gates AI input shaping
(built-in detectors plus your `regex_denylist`), so private content is
not sent to the model verbatim. **Public** / unclassified bodies are
embedded as-is. If you change which captures are Secret/Private, use
*Rebuild index* to re-embed under the new policy; the index also
rebuilds automatically when this embedding policy is revised.

The vector follows the entry's lifecycle the same way the rest of the
row does. A per-entry delete is a **soft delete**: the entry (and its
vector) stays in the file, filtered out of search results. A retention
sweep (count / age / size cap) and *Clear history* / clear-on-quit are
**hard deletes**: `ON DELETE CASCADE` drops the vector — along with the
body, blobs, and search index — in the same transaction, so the content
is physically removed from the live database rather than tombstoned. So
a vector persists at rest only after an *ordinary per-entry delete*; if
you need a soft-deleted entry's bytes gone immediately, enable
**Delete entries permanently** before deleting or run **Purge deleted
entries now** after deleting. Freed pages can still be recovered from
the raw file or a backup until reused or `VACUUM`ed — see [Data at
rest](#data-at-rest) — which is why full-disk encryption remains the
recommended at-rest protection.
Turning the toggle off stops indexing, and a model change (different
identifier, revision, or dimension) clears and rebuilds the index
rather than mixing incompatible embedding spaces.

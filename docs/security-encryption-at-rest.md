# Encryption at rest — investigation notes

Status: **not implemented**. This memo captures the trade-offs and open
questions so the work can be resumed without rediscovering them.

The current state is documented in README → Privacy and security and in
ARCHITECTURE.md §19: SQLite is forced to `0600` with a `0700` parent
directory, but the bytes on disk are plaintext. Permission bits keep
other local users out; they do not defend against anything running as
the same user (backups, sync clients, malware).

## Why encryption at rest is open

The clipboard inevitably captures secrets: passwords copied from a
manager, tokens pasted into a terminal, API keys lifted from `.env`
files. The `Store redacted` default scrubs *new* captures but cannot
retroactively rewrite the SQLite freelist or any backup snapshot.
Encryption at rest is the only mitigation that holds when a third
party gets file-level read access (a stolen laptop image, a sync
target gone wrong).

## Candidate approaches

### 1. SQLCipher

The closest off-the-shelf fit — SQLCipher is a SQLite fork that
encrypts the entire DB page-by-page with AES-256-CBC plus a
per-page HMAC-SHA512 (the v4 wire format also derives the page
key via PBKDF2-HMAC-SHA512) and exposes the unchanged SQLite API.

**Pros:**

- Transparent to the rest of `nagori-storage`. Most call sites
  would not need to change beyond opening the connection with a
  key via `PRAGMA key`.
- The page-level boundary is the right granularity for
  Nagori — every row carries equally sensitive content.
- Tooling (CLI, GUI editors) understands the format, which keeps
  the recovery story tractable.

**Cons / risks to investigate before adopting:**

- `rusqlite` does not bind SQLCipher out of the box. Either
  switch to `rusqlcipher` (third-party fork, smaller maintainer
  base) or vendor SQLCipher's amalgamation under a `sqlcipher`
  feature flag. Either choice adds vendor bytes and a C build
  dependency on every release target.
- Binary size impact: SQLCipher pulls in OpenSSL or BoringSSL
  unless we explicitly point it at libsodium / a Rust crypto
  crate via the `SQLITE_HAS_CODEC` shim. Measure before merging.
- Performance: page reads/writes go through AES-CBC. For our
  workload (small rows, frequent reads, FTS5 over short tokens)
  the overhead is usually ≤ 10 %, but FTS5 + ngram on 100 k
  entries needs to be benchmarked against the existing 80 ms
  budget.
- Licensing: SQLCipher community edition is BSD-style — fine for
  bundling. The Zetetic-distributed shared libraries on macOS /
  Windows installers come with their own terms; if we ship a
  static build we side-step that.

### 2. Application-layer encryption

Encrypt only the secret-bearing columns (`payload`, `representations`,
`html`, …) with a per-row AEAD (XChaCha20-Poly1305 via `chacha20poly1305`)
keyed by an OS-derived master key.

**Pros:**

- No DB-engine swap; pure Rust crypto path; no C build chain.
- Search index can stay plaintext, side-stepping the FTS5 vs.
  encrypted-pages question — though that may itself be a privacy
  regression, depending on how much can be inferred from tokens.

**Cons:**

- Custom code on the most security-sensitive code path. The C
  amalgamation in SQLCipher has had a decade of fuzzing; a
  bespoke envelope has not.
- Schema migrations get hairier: every encrypted column needs an
  IV column, an AEAD tag column, and a versioning byte so we can
  rotate algorithms later without losing the corpus.
- Doesn't protect against an attacker who can read the search
  index — FTS5 tokens leak the original words.

### 3. OS-managed encrypted storage

Defer encryption to the OS (FileVault, BitLocker, LUKS,
`Persistent Domain Encrypted` directories). README already
recommends this as the practical mitigation today.

**Pros:**

- Zero code. No migration to write. Recovery is the user's
  existing disk-encryption story.

**Cons:**

- Doesn't help users who *can't* turn on full-disk encryption
  (shared machines, work-issued laptops with policy locks).
- Doesn't protect against sync clients that copy the
  data directory into a cleartext cloud share.

Treat option 3 as the documented status quo; options 1 and 2 are
the candidates for a real implementation. Option 1 (SQLCipher) is
the leading candidate unless benchmarks show > 25 % regression on
the search budget.

## Key-store integration

Whichever option lands, the key has to live somewhere. Plain config
files defeat the purpose. The minimum viable wiring is:

| Platform | Backend                                  | Crate candidate                |
| -------- | ---------------------------------------- | ------------------------------ |
| macOS    | Keychain (generic password, `kSecClassGenericPassword`) | `security-framework`           |
| Windows  | DPAPI `CryptProtectData` user scope, or Credential Manager | `windows-sys` / `keyring`      |
| Linux    | Secret Service (`org.freedesktop.secrets`) via libsecret  | `secret-service` / `keyring`   |

`keyring-rs` already abstracts all three but pulls a transitive
DBus dependency on Linux. Evaluate against an in-tree adapter
behind `nagori-platform-{macos,windows,linux}` so the choice can
vary per OS without leaking into core.

Open questions for the key-store layer:

- **First launch:** generate a random key, write to the keystore,
  never expose it through the UI. No "show me the recovery
  phrase" — losing the keystore entry must mean losing the
  history, otherwise the whole exercise is theatre.
- **Multiple keys:** do we support per-device keys (laptop + desktop
  sync) or always a single device-local key? Sync is explicitly
  out of scope today; defer.
- **Keystore unavailable at launch:** the daemon must fail closed
  rather than silently downgrading to plaintext. The capture loop
  is allowed to refuse to start; the desktop shell needs an
  actionable error and a "retry" affordance.

## Migration plan (when we implement)

Destructive migrations are already the workspace convention
(see `crates/nagori-storage/src/sqlite.rs::MIGRATIONS`), but
*destroying* a clipboard history on upgrade is unacceptable. The
acceptable shapes:

1. **Opt-in switch.** New column in `settings`; default off; user
   flips it on in the Privacy panel. On flip, the daemon:
   - drains the in-memory queue and closes every open connection
     (capture, search, IPC) so no writer is holding the WAL,
   - runs `PRAGMA wal_checkpoint(TRUNCATE)` then `PRAGMA
     journal_mode = DELETE` on the plaintext DB to fold the WAL
     back into the main file and drop the `-wal` / `-shm`
     sidecars before re-opening as encrypted,
   - generates a key, writes it to the keystore,
   - re-creates the DB encrypted, copies rows over,
   - `VACUUM` the new encrypted DB, then `unlink` the plaintext
     `nagori.sqlite`, `nagori.sqlite-wal`, and `nagori.sqlite-shm`
     (the sidecars are the actual leak vector — secrets the user
     copied since the last checkpoint live in `-wal`, not in the
     main file). Document that `unlink` leaves freed disk blocks
     recoverable until the OS reuses them; users who need stronger
     guarantees should re-encrypt the host volume.
2. **Bulk import on first launch with encryption enabled.** Skip
   the in-place rewrite; require a manual export + import. Less
   code, worse UX. Probably the right MVP — and it side-steps the
   WAL/SHM sidecar question entirely because the plaintext DB is
   never opened after the user flips the switch.

`MIGRATIONS` itself stays plain — encryption is below the schema,
not part of it. The flip from plaintext-DB to encrypted-DB lives
in `NagoriStore::open`, gated on a new settings flag.

## Backup and recovery

- Encrypted DB + key in OS keystore is a hostile pair to back up:
  bare file copies are useless without the key, and the
  keystore is per-device. A `nagori export` that emits a
  user-passphrase-wrapped envelope (Argon2id KDF + XChaCha20-Poly1305)
  is the realistic recovery path.
- Backups must include the `nagori.sqlite-wal` sidecar (or run
  `wal_checkpoint(TRUNCATE)` before copying); otherwise the restore
  silently loses the last-N captures that hadn't been folded into
  the main file yet. Same applies to disk-image snapshots taken
  while the daemon is running.
- `nagori doctor` should gain a check that the keystore entry is
  reachable; a missing entry must page the user, not silently
  refuse to capture.
- Re-keying: keep the same DB, decrypt with old key, encrypt with
  new, swap atomically. Out of scope for the initial cut but the
  schema layout (separate IV/AEAD columns in option 2 above) has
  to allow it.

## Performance budget

Existing search budget: **top-50 under 80 ms for 100 k text
entries** (ARCHITECTURE.md §18). Encryption-at-rest must not
regress this past 100 ms or the palette gets perceptibly laggy.
Capture-side budget is looser (every clip is a single small write)
but should still fit in the existing 50 ms paste-delay window
the desktop honours.

Concrete experiments to run before deciding:

- SQLCipher vs. plain SQLite with the existing
  `crates/nagori-search` benches at 1 k / 10 k / 100 k entries on
  an M-series MacBook and a mid-tier Windows laptop.
- Cold-open latency: first connect after launch (SQLCipher derives
  the page key from the master key via PBKDF2 with 256 k rounds
  by default; that's measurable). `cipher_kdf_iter` is the knob.
- Memory: SQLCipher page cache is the same shape as SQLite's, but
  the working set grows because every read decrypts into a new
  page buffer. Track RSS at the 100 k corpus point.

## Recommended next steps

1. Land the docs (this memo + README section) — done.
2. Prototype SQLCipher behind a `--features sqlcipher` cargo flag
   in `nagori-storage`, with a feature-gated `open_encrypted`
   path. Keep it off by default so the CI matrix doesn't grow
   immediately.
3. Wire the keystore adapter in `nagori-platform-{macos,windows,linux}`
   behind a `KeystoreProvider` trait so the prototype can read /
   write keys without leaking the implementation into `core`.
4. Run the search benchmarks at 100 k entries on macOS and Windows;
   gate the decision on the ≤ 100 ms threshold.
5. Decide on the migration shape (opt-in flip vs. bulk export +
   import). Document in `ARCHITECTURE.md §7` before merging.

Until those steps land, the README guidance (rely on full-disk
encryption, prefer `Store redacted`) is the supported answer.

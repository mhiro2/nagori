# Architecture

How Nagori is structured: crate boundaries, runtime topology, and rules for
keeping the desktop palette, CLI, and capture daemon aligned.

---

## Table of contents

1. [Goals and constraints](#1-goals-and-constraints)
2. [Layers and crates](#2-layers-and-crates)
3. [Runtime topology](#3-runtime-topology)
4. [Capture pipeline](#4-capture-pipeline)
5. [Domain model](#5-domain-model)
6. [Dependency rules](#6-dependency-rules)
7. [Storage](#7-storage)
8. [Search](#8-search)
9. [Sensitivity and redaction](#9-sensitivity-and-redaction)
10. [Platform adapters](#10-platform-adapters)
11. [IPC boundary](#11-ipc-boundary)
12. [Tauri boundary and frontend](#12-tauri-boundary-and-frontend)
13. [CLI](#13-cli)
14. [Quick actions and AI actions](#14-quick-actions-and-ai-actions)
15. [Internationalization](#15-internationalization)
16. [Desktop shell integration](#16-desktop-shell-integration)
17. [Observability](#17-observability)
18. [Testing](#18-testing)
19. [Security notes](#19-security-notes)
20. [Product evolution](#20-product-evolution)
21. [Checklist for new work](#21-checklist-for-new-work)

---

## 1. Goals and constraints

Nagori is a **local-first clipboard history engine** with multiple delivery
surfaces (desktop palette, CLI, capture daemon).

**Central constraint:** domain and pipeline logic must stay
**target-agnostic**. No Tauri imports below the desktop shell, no macOS /
Windows / Linux APIs outside platform adapters, no SQLite dependency in
domain code. This leads to four design rules:

1. **Explicit intermediate models** — snapshot → entry → search document →
   ranked result, each testable in isolation.
2. **Thin surfaces** — Tauri commands, CLI subcommands, and IPC handlers
   deserialize requests, call into `nagori-daemon::NagoriRuntime`, and
   serialize results.
3. **DTO-style boundaries** — public APIs expose Nagori-owned types; raw
   `rusqlite::Row`, `NSPasteboard`, and Tauri `tauri::State` never leak
   through.
4. **Same runtime for every surface** — the palette, the CLI's `--ipc` /
   `--auto-ipc` mode, and the standalone daemon all drive the same
   `NagoriRuntime` so capture, search, paste, and Quick actions behave
   identically regardless of entry point.

---

## 2. Layers and crates

```text
┌──────────────────────────────────────────────────────────┐
│ Surfaces                                                 │
│   apps/desktop (Tauri + Svelte)     nagori-cli           │
└────────────────────────────┬─────────────────────────────┘
                             │
                             ▼
┌──────────────────────────────────────────────────────────┐
│ Runtime                                                  │
│   nagori-daemon — capture loop, maintenance, IPC server, │
│                   search cache, runtime façade           │
└────────────────────────────┬─────────────────────────────┘
                             │
         ┌───────────────────┼───────────────────┐
         ▼                   ▼                   ▼
┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐
│ Domain / logic   │ │ Storage / search │ │ Adapters         │
│ nagori-core      │ │ nagori-storage   │ │ nagori-platform  │
│                  │ │ nagori-search    │ │ nagori-platform- │
│                  │ │                  │ │   {macos,        │
│                  │ │                  │ │    windows,      │
│                  │ │                  │ │    linux,        │
│                  │ │                  │ │    native}       │
│                  │ │                  │ │ nagori-ai        │
│                  │ │                  │ │ nagori-ipc       │
└──────────────────┘ └──────────────────┘ └──────────────────┘
```

| Crate | Role |
|-------|------|
| `nagori-core` | Domain model, sensitivity policy, repository traits, `SearchService` orchestration, settings, errors |
| `nagori-storage` | SQLite (rusqlite) repositories, FTS5 / ngram tables, migrations, image blob handling, and the `sqlite-vec`-backed semantic embedding index (`semantic-index` feature) |
| `nagori-search` | Text normalization, CJK n-gram tokenizer, default ranker |
| `nagori-platform` | Cross-platform traits: clipboard read/write, paste, hotkey, permissions, frontmost window |
| `nagori-platform-macos` | NSPasteboard capture, Cmd+V auto-paste, Accessibility checks, frontmost-app metadata |
| `nagori-platform-windows` | Win32 clipboard capture (`GetClipboardSequenceNumber` + arboard text + arboard image RGBA → PNG re-encode with a CF_DIBV5 / CF_DIB / registered-PNG availability probe + `CF_HDROP` file lists), text + image + file-list copy-back (PNG → RGBA via arboard, file paths packed into a hand-rolled `DROPFILES` + `SetClipboardData(CF_HDROP)`), `SendInput` Ctrl+V auto-paste, `GetForegroundWindow` frontmost-app probe; hotkey registration delegated to Tauri shell |
| `nagori-platform-linux` | Wayland-only Linux adapter — `wl-clipboard-rs` clipboard over `wlr_data_control` / `ext_data_control` (no X11 fallback) with multi-MIME enumeration (text, image PNG/JPEG/GIF/WebP/TIFF, `text/uri-list` file lists), text + image + file-list copy-back (`image::guess_format` → `copy::MimeType::Specific`, RFC-2483 URI-list serialisation via `url::Url::from_file_path`) and a `copy::copy_multi` Preserve transaction that offers text / HTML / image / `text/uri-list` simultaneously, `wtype` Ctrl+V auto-paste, frontmost-app probe unsupported (no Wayland API exposes it); hotkey registration is delegated to the Tauri `tauri-plugin-global-shortcut` shell (X11-only — fails with `Unsupported` on a pure Wayland session) |
| `nagori-platform-native` | Per-OS adapter wiring shared by `nagori-cli` (daemon + direct copy/paste) and `apps/desktop`. `build_native_runtime(store, options)` returns a `NagoriRuntime` plus the auxiliary clipboard reader / window handles, picking the right concrete `nagori-platform-{macos,windows,linux}` adapter at compile time. Centralises the Linux Wayland error annotation so both call sites surface the same compositor-requirement hint. |
| `nagori-ai` | Cross-platform AI engine: the `AiActionEngine` trait + `AiEngine`, the `(action, provider) → backend` resolver, the `TextGenerator` / `Translator` / `Embedder` backend traits, a deterministic `MockBackend`, the rule-based quick-action runner, and the redactor. No platform deps |
| `nagori-ai-apple` | macOS-only Apple on-device AI bridge. Isolates the Swift / FoundationModels / Translation / NaturalLanguage build/link deps behind a Swift static library: `AppleFoundationBackend` (a `nagori-ai` `TextGenerator` that streams on-device text — summaries, rewrites, Markdown reformatting, task extraction, code explanations — via `SystemLanguageModel`), `AppleTranslateBackend` (a `Translator` over `TranslationSession` with `NLLanguageRecognizer` source detection), `AppleEmbedderBackend` (an `Embedder` over `NLContextualEmbedding` for semantic search), Apple Intelligence availability probe (with cross-platform mock fixtures), longest-common-prefix delta-isation of partial snapshots, and a Tokio-mpsc stream with cancellation |
| `nagori-ipc` | Newline-delimited JSON over a per-platform transport (Unix domain socket on Unix, Win32 named pipe on Windows); auth-token handshake, request/response DTOs |
| `nagori-daemon` | `NagoriRuntime` façade, capture loop, maintenance jobs, the background semantic-index worker, IPC server, in-memory search cache |
| `nagori-cli` | `nagori` binary; clap commands, plain/JSON/JSONL output, IPC client + read-only DB fallback |
| `apps/desktop` | Tauri 2 shell + Svelte 5 frontend; thin command layer over `NagoriRuntime`. `AppState::build` delegates platform adapter selection to `nagori-platform-native::build_native_runtime`, so the Linux Wayland missing-`wl_data_control` hint is shared with the CLI daemon path. The system tray (macOS menu bar / Windows notification area / Linux StatusNotifierItem), palette commands, autostart, global-shortcut registration and updater plugin are wired on every OS; capabilities that genuinely cannot exist off macOS (secure-input detection, sleep/wake pasteboard-sequence handling, X11-only global hotkeys on a pure Wayland session) remain `Unsupported` and surface to the UI as such. |

Repository layout (abbreviated):

```text
apps/
  desktop/                  # Tauri + Svelte palette and settings UI
crates/
  nagori-core/ nagori-storage/ nagori-search/
  nagori-platform/ nagori-platform-macos/
  nagori-platform-windows/ nagori-platform-linux/
  nagori-platform-native/
  nagori-ai/ nagori-ai-apple/ nagori-ipc/ nagori-daemon/ nagori-cli/
docs/                       # CLI / IPC / permissions / release notes
```

---

## 3. Runtime topology

`NagoriRuntime` is the shared façade: it holds the SQLite store, the
search-cache handle, the settings broadcast channel
(`tokio::sync::watch`), and the AI / clipboard / paste adapters built
through `NagoriRuntimeBuilder`. Long-running work (capture loop,
maintenance jobs, settings subscribers, IPC accept loop) is spawned by
the **host** — the Tauri shell (`apps/desktop/src-tauri/src/state.rs`)
or `run_daemon` in `nagori-daemon::serve` — onto the host's tokio
executor, with the runtime handed in by reference. Surfaces attach to
the same runtime instance:

```text
apps/desktop (Tauri)
  ├─ Svelte WebView UI
  ├─ Tauri commands  ────► NagoriRuntime ──► SqliteStore
  ├─ tray + autostart      AppState spawns:    + search cache
  └─ AppState              · capture loop      + action runner
       └ spawns tasks      · settings sub      + platform adapters
                           · maintenance

nagori-cli (`--ipc` / `--auto-ipc`)
  └─ IpcClient ──► Unix socket / named pipe ──► IpcRequest / IpcEnvelope
                                       │
nagori-cli `daemon run`                ▼
  └─ run_daemon ──► accept_loop ──► NagoriRuntime.handle_ipc
```

Two execution modes:

- **In-process (desktop)** — the Tauri shell builds a `NagoriRuntime`
  via `NagoriRuntimeBuilder` and Tauri commands call its methods
  directly. `AppState::spawn_background_tasks` and
  `spawn_settings_subscribers` (`apps/desktop/src-tauri/src/lib.rs`)
  start the capture loop and settings fan-out; the same five background
  workers (capture, maintenance, semantic index, ngram backfill, AI
  stale-request watchdog) run under the same `supervise_worker` policy as
  the daemon (see below), so a panic no longer leaves the app running with
  a dead loop. `AppState::try_new_at`
  first takes the single-instance lock (`nagori_storage::ProcessLock`
  over the DB directory) before opening the store, so a second launch —
  or a standalone daemon sharing the same data directory — is refused
  rather than running migrations and a second capture loop against the
  same store. A duplicate launch surfaces the refusal in the startup
  fallback window; see [§11](#11-ipc-boundary) for the lock semantics
  shared with the daemon.
- **Out-of-process (daemon + CLI)** — `nagori daemon run` calls
  `nagori-daemon::serve::run_daemon`, which spawns the same kind of
  background tasks plus an IPC accept loop (Unix-domain socket on
  macOS / Linux, named pipe on Windows), then dispatches every request
  through `NagoriRuntime::handle_ipc`. Each long-running worker (capture,
  maintenance, semantic index, AI stale-request watchdog) runs under
  `supervise_worker`: a panic or
  unexpected early return — while shutdown was *not* requested — is logged
  and the worker is respawned after an exponential backoff, so a crashed
  loop can no longer leave the daemon serving with a dead worker and a
  stale health snapshot. The one-shot ngram backfill is supervised too,
  but only respawns on a panic (a clean completion is terminal). On
  shutdown each supervisor drains its worker within the grace window,
  force-aborting one wedged in an un-cancellable `spawn_blocking`. The IPC
  accept loop keeps its own supervisor (backoff restart + liveness probe).
  CLI calls with
  `--ipc <endpoint>` / `--auto-ipc` route through that transport;
  `--db <path>` is a read/write fallback that bypasses the daemon and
  is documented as **repair / offline mode** in
  [`docs/cli.md`](./docs/cli.md).

The two surfaces speak different DTOs on purpose: the daemon serves
`IpcRequest` / `IpcResponse` (`nagori-ipc::protocol`) wrapped in an
authenticated `IpcEnvelope`, while Tauri commands return camelCase
DTOs from `apps/desktop/src-tauri/src/dto.rs`. Both call the same
`NagoriRuntime` methods, so capture, search, paste, and AI behaviour
stay identical regardless of entry point.

---

## 4. Capture pipeline

```text
ClipboardReader.current_sequence_with_max()
                                            (cheap pre-check; byte-read
                                             adapters receive the entry cap)
  → frontmost_app() snapshot               (before reading the body)
  → frontmost_focused_is_secure()          (AX kAXSecureTextField guard;
                                            true → audit + drop without
                                            touching the body)
  → ClipboardReader.current_snapshot_with_max()
                                            (pre-read size guard where
                                             platform supports it)
  → EntryFactory.from_snapshot()           (decode → ClipboardEntry +
                                            SHA-256 content hash +
                                            search document +
                                            pending_representations for
                                            every validated rep)
  → kind guard  (settings.capture_kinds)
  → primary-size guard (settings.max_entry_size_bytes)
  → SensitivityClassifier.classify()       (built-in detectors +
                                            app_denylist + user regexes)
        ├─ Blocked → audit + drop
        └─ otherwise → take redacted preview
  → SecretHandling
        ├─ Block         → audit + drop
        ├─ StoreRedacted → rewrite body / hash / FTS / ngrams
        └─ StoreFull     → keep raw bytes
  → Secret? clear pending_representations   (alternatives still hold the
                                             raw secret — drop them and
                                             reset representation_set_hash
                                             to mirror content_hash)
  → trim_alternatives_to_budget             (enforce
                                             max_entry_size_bytes over the
                                             full rep set; recompute
                                             representation_set_hash)
  → search-cache invalidate (pre)
  → EntryRepository.insert()               (single SQLite tx writes
                                            entries + entry_representations
                                            + search_documents + ngrams;
                                            search_fts stays in sync via
                                            AFTER INSERT/DELETE/UPDATE
                                            triggers on search_documents)
  → search-cache invalidate (post)
```

Notes (`crates/nagori-daemon/src/capture_loop.rs`,
`crates/nagori-core/src/factory.rs`,
`crates/nagori-core/src/policy.rs`):

- The sequence pre-check short-circuits before the body is read, so
  duplicate captures cost a single pasteboard round-trip.
- Frontmost app metadata is captured **before** the clipboard body so
  `Cmd+C → Cmd+Tab → paste` flows still attribute the source correctly
  to the password manager / denylisted app.
- `EntryFactory` performs decoding + content-hash + search-document
  construction; it does **not** consult settings. Size cap and
  classification both live in the capture loop. Code-kind bodies are
  given a canonical `language_hint` (`json` / `rust` / `sql` / …) by the
  dependency-free `model::code_language::detect`; the hint flows into
  `SearchDocument::language` and downstream into the preview highlighter,
  the result-row language badge, and the ranker.
- Image entries get their pixel `width`/`height` from a **header-only**
  probe (`image::ImageReader::into_dimensions`) run in the capture loop
  just before insert — `nagori-core` deliberately has no `image`
  dependency, so the factory leaves both `None`. The probe is bounded by
  `MAX_DECODED_IMAGE_PIXELS` (a forged-dimension guard) and fails open to
  `None`, so capture never stalls on an unreadable header. Pre-probe rows
  keep `None` and degrade gracefully in the UI.
- `app_denylist` is enforced inside `SensitivityClassifier::classify`
  against the snapshot's `source` (bundle id / name), not in the
  factory.
- `EntryRepository::insert` upserts `entries`, `search_documents`, and
  `ngrams` in one SQLite transaction, so search is consistent the
  moment the row commits — there is no separate
  `SearchRepository.upsert_document` step in the live path. The
  `search_fts` virtual table is an FTS5 external-content index over
  `search_documents` and is kept in sync by `AFTER INSERT/DELETE/UPDATE`
  triggers, so application code only touches the content row.
- The cache is invalidated on **both** sides of the insert; see
  [section 8](#8-search) for why.
- A wall-clock gap of ≥ 30 s between two `capture_once` invocations is
  treated as a host-paused signal (sleep / suspend / lid close) and arms
  a one-shot dedupe-hash cross-check on the next tick: even if the
  observed sequence still matches `last_sequence`, the body is read and
  the new dedupe hash (the entry's `representation_set_hash` when
  present, else `content_hash`, matching the storage layer's dedupe
  key) is compared against the last captured one before any insert
  decision. macOS can lap the pasteboard `changeCount` silently across
  a sleep cycle, so without this defence a fresh post-wake clip whose
  sequence happens to collide with the pre-sleep value would be skipped
  as a duplicate. The detector uses `SystemTime` rather than `Instant`
  because Darwin's `Instant` is `CLOCK_UPTIME_RAW` and stops while the
  system sleeps. The pristine launch path under
  `capture_initial_clipboard_on_launch=false` also anchors the dedupe
  hash so a post-wake resync without any user copy correctly recognises
  the unchanged pre-launch clipboard and does **not** promote it. That
  baseline read goes through `current_snapshot_with_max` instead of the
  unbounded `current_snapshot`, so a huge pre-launch text/image is bounded
  (and the internal decoded-pixel cap applies) rather than fully
  materialised just to seed the dedup state. It is bounded by the internal
  hard limit (`MAX_ENTRY_SIZE_BYTES`), not the live `max_entry_size_bytes`:
  since the setting can never exceed that hard limit, any clip that could
  ever become capturable — even after the user later raises the setting —
  is within this read and gets its dedup hash anchored, so a post-raise
  wake-resync still recognises it. A clip over the hard limit can never be
  captured under any setting, so it anchors only the sequence and is
  skipped.
- After frontmost is captured, the loop asks the platform whether the
  frontmost app's currently-focused element is a secure text field
  (`kAXSecureTextField` role/subrole). When true, the clip is dropped
  before the body is even read so password-input keystrokes never reach
  storage. A single AX error fails open (treated as "not secure") so a
  transient FFI hiccup doesn't stall capture; sustained errors past
  `SECURE_FOCUS_FAIL_CLOSED_THRESHOLD` flip to fail-closed (assume
  secure, skip capture) on the assumption that a permanent outage means
  Accessibility was revoked or the AX subsystem is wedged. A
  `SECURE_FOCUS_BUNDLE_OVERRIDES` list also forces fail-closed when the
  frontmost is a known system password UI (e.g. `com.apple.SecurityAgent`)
  whose AX state is deliberately scrubbed. The
  `SensitivityClassifier` secret detector and password-manager bundle
  denylist still run as the second line of defence. The macOS impl
  bounds the per-element AX trip via `AXUIElementSetMessagingTimeout`
  so an unresponsive focused process can't stall the polling tick.
  Test harnesses that can't grant Accessibility programmatically can
  set `NAGORI_DISABLE_SECURE_FOCUS_FAIL_CLOSED=1` (the parser accepts
  `1`/`true`/`yes`/`on`, case-insensitive) to keep the loop failing open
  through sustained AX errors; the bundle-override list still applies.
  Intended for `scripts/e2e-macos.sh`; production runs leave the default
  fail-closed behaviour.

The pipeline is purely async over `tokio`; the macOS adapter polls
NSPasteboard with backoff and reports a `ClipboardSequence` so the loop
short-circuits without re-reading data.

---

## 5. Domain model

Types live in `nagori-core` (`model.rs`, `settings.rs`, `policy.rs`,
`services/search.rs`).

**`ClipboardEntry`** — the unit of history. Wraps `ClipboardContent`,
`EntryMetadata`, `SearchDocument`, `Sensitivity`, and `EntryLifecycle`.

**`ClipboardContent`** — kind-tagged enum:
`Text` / `Url` / `Code` / `Image` / `FileList` / `RichText` / `Unknown`.

- `TextContent` / `UrlContent` / `CodeContent` carry plain strings plus
  derived metadata (counts, normalized URL, language hint). `CodeContent`'s
  `language_hint` is filled by `model::code_language::detect` at
  classification time (see [section 4](#4-capture-pipeline)); minified
  single-line JSON also tips `looks_like_code` so it lands as `Code` with a
  `json` hint rather than plain `Text`.
- `ImageContent` carries optional in-memory `pending_bytes` that flow
  from capture → factory → storage; after insertion the bytes live in
  `entry_representations.payload_blob` (the `role = 'primary'` row owned
  by the entry) and the field is always `None` post-deserialisation. Its
  `width`/`height` are populated by the capture-loop header probe (see
  [section 4](#4-capture-pipeline)), not the factory.
- `RichTextContent` keeps `plain_text` (for FTS / ngrams) and an optional
  `markup` payload tagged `Html` or `Rtf` for preview rendering.
- `FileListContent` flattens `NSPasteboardTypeFileURL` URLs into POSIX
  paths plus a `display_text` newline-joined form for search.

Payload storage is uniform: every captured representation (primary,
plain fallback, alternative) writes one row in `entry_representations`
with either inline `text_content` or a binary `payload_blob`. The
domain model no longer needs a per-kind storage discriminator — the
representation rows are the source of truth.

**`EntryMetadata`** — timestamps, source app (`bundle_id`, `name`,
`executable_path`), use count, `ContentHash` (SHA-256 over the primary
body — kept as a stable fingerprint for telemetry and for entries that
never built a representation set), and `representation_set_hash` (the
actual dedupe key the storage layer enforces). The set hash is SHA-256
over a canonical encoding of every persisted representation
(`role|mime|ordinal|sha256(payload)` rows, joined with newlines after
sorting), so dedup can recognise "same representation set" separately
from "same primary body". Snapshot-derived entries carry a canonical set
hash computed by `factory::compute_representation_set_hash`, even for a
single-representation set. Rows that don't own a live representation set
fall back to mirroring `content_hash` so the column stays populated:
synthesised entries that never built one (CLI `add_text`), and Secret
entries whose alternatives the capture pipeline cleared during
classification (the reset is what stops a redacted body from being
fingerprinted against the original markup).

**`pending_representations`** (`Vec<StoredClipboardRepresentation>`) —
an in-memory-only field on `ClipboardEntry` (`#[serde(skip)]`) that
carries every allowlisted, magic-number-validated, non-empty
representation a snapshot produced. The factory fills it in
`EntryFactory::from_snapshot`; the storage layer drains it into
`entry_representations`; the field is always empty after a database
round-trip (the persisted rows are the source of truth). Each entry
records a `role` (`Primary` / `PlainFallback` / `Alternative`), a
canonical `mime_type`, an `ordinal`, and a typed `data` (`InlineText`,
`DatabaseBlob`, or `FilePaths`). The capture pipeline runs
`ClipboardEntry::trim_alternatives_to_budget` so the user's
`max_entry_size_bytes` covers the full set; the primary is preserved
even if oversized — that case is rejected upstream by the
`payload_bytes` guard before classification.

**`SearchDocument`** — title, preview, `normalized_text`, tokens,
language. Indexed by both FTS5 and the ngram table.

**`Sensitivity`** + **`SensitivityReason`** — policy classification
(`Unknown` / `Public` / `Private` / `Secret` / `Blocked`) plus the
detector that triggered (`ApiKeyPattern`, `JwtPattern`, …).

**`EntryLifecycle`** — `pinned`, `archived`, `deleted_at`, `expires_at`.

**`ClipboardSnapshot`** + **`ClipboardRepresentation`** — raw OS read.
The platform adapter returns this; the factory turns it into a domain
entry.

**`SearchQuery`** / **`SearchMode`** / **`SearchFilters`** /
**`SearchResult`** / **`RankReason`** — query DTOs and result scoring
metadata. `SearchMode::Auto` lets the planner pick an FTS, ngram, or
hybrid path.

**`AiAction`** / **`AiActionId`** / **`AiInputPolicy`** /
**`AiOutput`** — AI surface types. `AiInputPolicy` governs whether a
remote provider may see the raw entry, whether redaction is required,
and the maximum payload size.

Refer to the source for exact field shapes; this document does not
duplicate them.

---

## 6. Dependency rules

```text
apps/desktop ──► nagori-daemon ──┐
              ├► nagori-ai       │
              ├► nagori-core     ├──► nagori-core
              ├► nagori-ipc      │
              ├► nagori-platform │
              ├► nagori-search   │
              └► nagori-storage  │
                 nagori-platform-macos (target = macOS)
                                  │
nagori-cli ──► nagori-ipc ────────┤
            ├► nagori-daemon (only when hosting the daemon)
            └► nagori-core / storage / search / platform / ai

nagori-daemon ──► nagori-core / storage / search / platform / ai / ipc

nagori-storage / nagori-search / nagori-ai ──► nagori-core
nagori-platform-{macos,windows,linux} ──► nagori-platform ──► nagori-core
```

- `nagori-core` must not depend on Tauri, SQLite, OS APIs, or AI provider
  SDKs.
- `nagori-storage` and `nagori-search` both depend on `nagori-core`, and
  may depend on each other for FTS integration. The direction is one-way:
  domain logic stays unaware of SQLite specifics.
- `nagori-platform` defines traits only. Platform-specific code lives in
  `nagori-platform-{macos,windows,linux}` and is selected through target
  gates in the host's `Cargo.toml`.
- `nagori-daemon` composes everything. The desktop shell **also** depends
  directly on `nagori-core`, `nagori-storage`, `nagori-search`,
  `nagori-platform`, `nagori-ipc`, and `nagori-ai` because the Tauri
  command layer wires its own DTOs and uses platform traits (e.g.
  `state.window.activate_app()`) outside the runtime façade. CLI depends
  on `nagori-ipc` for the wire protocol and on `nagori-daemon` only when
  running as a daemon host.
- `nagori-ai` depends only on `nagori-core` so providers stay swappable.

---

## 7. Storage

**Engine:** SQLite via `rusqlite` with a single connection pool, WAL
journal mode, and `synchronous = NORMAL`. The `SqliteStore` exposes both
the repository trait impls and a `SearchCandidateProvider` for
`SearchService`.

**Schema versioning:** `PRAGMA user_version` plus a static
`MIGRATIONS: &[(i64, &str)]` table inside `nagori-storage`. Migrations
are forward-only; downgrades are not supported.

**Tables:**

| Table | Purpose |
|-------|---------|
| `entries` | Full entry rows. Owns metadata only; the payload bytes (text, image, etc.) live in `entry_representations`. The `representation_set_hash` column carries the joint hash of the entry's representations and is the unique dedupe key over live rows, so two snapshots with the same primary text but different HTML/RTF/file-list alternatives land in distinct rows. The denormalised `total_byte_count` column is maintained by triggers on `entry_representations` so the retention/byte-budget paths read a single column instead of joining and summing. |
| `entry_representations` | Per-representation payload rows owned by an entry (`entry_id` FK with `ON DELETE CASCADE`). Each row carries `role`, `mime_type`, `platform_format`, `ordinal`, exactly one of inline `text_content` or `payload_blob`, and a denormalised `byte_count` used by the retention budget. Snapshot captures persist one row per validated representation: `role = 'primary'` for the chosen body, `role = 'plain_fallback'` for the sibling `text/plain` of a paired `RichText`, and `role = 'alternative'` for the remainder (HTML, RTF, image bytes, file URLs). Synthesised entries (CLI `add_text`, redacted Secret rows) still write a single `primary` row. `(entry_id, role, ordinal)` is unique. |
| `entry_thumbnails` | Derived 512px raster previews for `Image`-kind entries (and file lists that carried an accompanying image render) (JPEG for opaque sources, PNG for sources carrying alpha so transparent pixels survive into the inline preview), keyed by `entry_id` with `ON DELETE CASCADE`. Strictly a cache: rows are generated lazily on the first preview request, are regenerable from the primary representation, and an LRU sweep keyed on `last_accessed_at` (touched on every `get_thumbnail` hit) enforces `AppSettings::max_thumbnail_total_bytes` (default 64 MiB). Kept in a dedicated table (rather than in `entry_representations`) so a paste / copy-back can never accidentally hand a downscaled raster to the host clipboard. |
| `search_documents` | Title, preview, normalized text per entry — the source of truth for what FTS / ngrams index. Carries an explicit `doc_id INTEGER PRIMARY KEY` so the rowid is stable across `VACUUM` and the FTS5 external-content pointer remains valid. `ngram_index_version` records which gram-generator revision built the row's grams so an upgrade can rebuild stale rows in the background. |
| `search_fts` | FTS5 external-content virtual table (`content = 'search_documents'`, `content_rowid = 'doc_id'`) over `title` / `preview` / `normalized_text` (`unicode61`). Kept in sync by `AFTER INSERT/DELETE/UPDATE` triggers on `search_documents`; application code never writes to it directly. |
| `ngrams` | `(gram, entry_id, position)` triples for CJK partial-match lookup, capped at `MAX_NGRAM_INPUT_CHARS` (4096) characters per entry. `entry_id` FK to `entries(id)` with `ON DELETE CASCADE` so hard-deletes don't leak posting rows. |
| `entry_embeddings` | On-device semantic-search vectors (`semantic-index` feature). One row per entry: a little-endian float32 `vector` BLOB ranked by `sqlite-vec`'s `vec_distance_cosine`, the runtime `dimension`, and the source `content_hash`. `entry_id` FK with `ON DELETE CASCADE`. A per-entry delete is *soft* (the vector stays in the file, filtered out at query time), while retention sweeps and *Clear history* / clear-on-quit *hard-delete* the entry, so the cascade drops its vector in the same transaction. |
| `semantic_index_meta` | Singleton row recording the embedding model (`model_identifier` / `revision` / `dimension` / `max_sequence_length` / `index_version`) the stored vectors were produced with, so a model change clears the index and triggers a rebuild instead of mixing incompatible spaces. |
| `settings` | Key/value persistence for `AppSettings`. |
| `audit_events` | Capture / policy events (block, redact, etc.). Never stores raw clipboard content. |

**Image bytes** stay inline (in the primary `entry_representations` row)
because typical clipboard images are sub-MiB and SQLite handles that
size cheaply; flowing them through a content-addressed file store was
not worth the extra failure modes for the size class. The frontend
streams them lazily via the `nagori-image://` Tauri custom URI scheme so
the WebView fetches `nagori-image://localhost/<entry_id>` like any other
`<img src>`. The handler returns 403 for
`Sensitivity::Private | Secret | Blocked` so secret imagery never
reaches the WebView.

**Thumbnails.** Inline preview rows fetch
`nagori-image://localhost/thumb/<entry_id>` instead of the original
payload — the daemon's `nagori-daemon::thumbnails` module decodes,
downscales to 512px, and re-encodes capped at 256 KiB per row. The
encoder branches on the source's alpha channel: opaque images take a
JPEG path (quality 85, then 60 on overflow); images carrying alpha
take a PNG path so transparent pixels survive into the inline
preview and the expanded view of the original payload shows the same
picture. A header-only dimension probe rejects encoded payloads whose
advertised canvas would breach `MAX_DECODED_IMAGE_PIXELS` so a forged
PNG IHDR cannot force the decoder to materialise a multi-GB buffer.
Generation is gated two ways before any task is spawned: an in-memory
`HashSet<EntryId>` collapses a burst of opens on the *same* entry to one
decoder, and a global decode-pool semaphore admits the request only if a slot
is free — both are acquired *before* `tokio::spawn`, so a burst of misses
against *distinct* entries (an image-heavy scroll, a prefetch sweep) is bounded
to the pool size instead of piling up detached tasks each parked on the
semaphore and each ready to allocate a large decode buffer. A rejected request
is retried on the next fetch (the `503` path below) once a slot frees. The same
`is_text_safe_for_default_output` sensitivity check that gates the
original-payload scheme handler is re-asserted inside the generator
before the thumbnail is written so a Private / Secret / Blocked
entry never produces a derived artifact. The thumbnail source is
normally the entry's primary image payload; a file list keeps its
primary as the joined paths (text), so for that kind the generator
falls back to an `image/*` representation the clip carried alongside
the file URLs (e.g. a presentation copied from Finder that also placed
a slide render on the clipboard). The file-list preview pane surfaces
that as a small supplementary thumbnail; the fallback reuses the same
sensitivity gate and signature validation, so it adds no new exposure.
The scheme handler returns
`503 Service Unavailable` + `Retry-After: 1` on miss; the frontend
re-fetches once on a fixed cadence (the `<img onerror>` event exposes
neither the status code nor headers), and on a second miss falls
back to the original payload URL so the row still renders. The
`MaintenanceService` applies `enforce_thumbnail_budget` after the
regular retention sweep, evicting the least-recently-accessed rows
first.

**Retention budget.** `enforce_total_bytes` sums every live entry's
denormalised `total_byte_count` (maintained by triggers on
`entry_representations`, so the budget total is a single-table
aggregate over the live partition rather than a JOIN+SUM) and evicts
oldest-first when the budget is exceeded. Eviction — like the count /
age sweeps and *Clear history* / clear-on-quit — **hard-deletes** the
parent `entries` row, so `ON DELETE CASCADE` (plus `recursive_triggers`
firing the `search_documents_ad_fts` sync trigger) drops the row's
representations, blobs, embeddings, thumbnails, and search / ngram
index in the same transaction. Retention therefore reclaims disk (a
later `VACUUM` from the maintenance sweep then shrinks the file) rather
than tombstoning rows that grow it forever. Per-entry deletes
(`delete_entry`) stay soft.

Hard-delete reclaims the rows; `secure_delete = ON` (set on every pooled
connection) zeroes their freed pages so the content is not recoverable
from the freelist, and the explicit purge paths (`clear_non_pinned`,
`clear_older_than`) follow up with `wal_checkpoint(TRUNCATE)` so the
pre-deletion bytes do not survive in historical WAL frames. This is
residue reduction inside the file, **not** encryption — see
[section 19](#19-security-notes).

**At-rest protection:** the database file mode is forced to `0600` and
the parent directory to `0700` on creation. The DB itself is **not**
encrypted — see [section 19](#19-security-notes).

---

## 8. Search

**Pipeline.** `SearchService` (in `nagori-core::services::search`)
orchestrates a `SearchCandidateProvider` (storage primitive) and a
`Ranker`. `SqliteStore` provides both an inherent `search()` shortcut
and the `SearchCandidateProvider` impl; `nagori-search::DefaultRanker`
supplies the scoring used by the daemon and tests.

**Plans.** `SearchPlan::try_resolve(mode, normalized)` chooses between
`Recent` (empty query), exact substring, FTS, ngram, and hybrid plans.
Hybrid plans fan substring / FTS / ngram candidate fetches out
concurrently via `tokio::try_join!`, and the storage layer hands each
branch its own pooled SQLite connection so they run truly in parallel
under WAL — readers do not block each other and an in-flight capture
write does not stall search.

**Bounded admission + cancellation.** Each candidate fetch runs through
`SqliteStore::run_search_blocking`, which adds two guards over the plain
blocking path. *Bounded admission*: a search-admission semaphore
(`POOL_CAPACITY - 1` permits) is held inside the blocking closure until the
pooled connection is returned, so concurrent — possibly superseded —
fan-outs can't claim every pooled connection and starve capture /
maintenance writes (the reservation holds even for an abandoned query whose
future was dropped). *Real cancellation*: `SearchService::search` owns a
`CancellationToken` whose drop guard fires when the search future is dropped
(a superseded keystroke, or a sibling branch failing the `try_join`); a
`sqlite3_progress_handler` installed on the connection polls that token
throughout statement execution and aborts the LIKE / FTS / ngram query so it
stops and releases its connection promptly instead of running to completion.
The handler is checked for the statement's whole life (closing the race an
after-the-fact `sqlite3_interrupt` would lose) and an RAII guard removes it
before the connection returns to the pool — even on a panic — so a later
borrower of the recycled connection is never aborted by a stale token.

**Hybrid ngram is CJK-scoped.** In the implicit `Hybrid` (Auto) plan the
ngram branch only fires for queries that carry a CJK character, and the
orchestrator passes `NgramQueryMode::CjkOnly` so the provider keeps just
the grams that contain a CJK char. ASCII word recall is already served by
FTS (whole-token) plus the bounded substring scan, while common ASCII
bigrams own huge posting lists whose `gram IN (...)` union explodes on
large histories. Filtering to CJK grams preserves CJK and mixed-script
recall while shedding that cost; a pure-ASCII Auto query skips ngram
entirely. ASCII partial / typo recall therefore lives in explicit
`SearchMode::Fuzzy`, which still runs the full gram set
(`NgramQueryMode::Full`).

**Semantic plan.** `SearchMode::Semantic` resolves to its own plan but
needs a query embedding, so `NagoriRuntime::search` routes it ahead of the
text pipeline: it embeds the query through the wired `Embedder` and ranks
the stored vectors via `SqliteStore::semantic_search` (`sqlite-vec` cosine
distance). The embedder is macOS-only and the index is opt-in, so when it
is disabled or unavailable the runtime returns `Unsupported`; a direct
store/test caller with no embedder gets an empty result set rather than an
error. See [section 14](#14-quick-actions-and-ai-actions) for the index
build pipeline.

**Substring scan window.** Hybrid plans bound their substring branch to
the most recent `SUBSTRING_SCAN_WINDOW` (5000) live entries via a CTE,
so tail latency for typical typing-driven searches stays roughly
constant as the history grows. `SearchMode::Exact` and
`SearchMode::Fuzzy` deliberately scan the full corpus — they exist for
explicit lookups where completeness beats latency.

**Recent-search cache.** A bounded LRU
(`nagori-daemon::search_cache::RecentSearchCache`, default capacity 32)
sits in front of `SearchService` inside `NagoriRuntime::search`. Only
queries up to `CACHEABLE_QUERY_LEN` (8) characters — the empty `Recent`
plan and the first few keystrokes — are cached, so the working set stays
tiny while the hottest paths skip the SQLite round-trip. The cache key
normalises `SearchFilters.kinds` (sorted + deduped) so semantically
equivalent filter sets share a slot.

**Cache invalidation.** Every corpus mutation invalidates the cache
**before and after** the storage write. `add_text`, `copy_entry`
(use-count bump), `delete_entry`, `pin_entry`, and the `Clear` IPC
handler call `invalidate_search_cache` directly; the capture loop and
maintenance service are wired with `with_search_cache` so successful
captures and retention sweeps invalidate through the same handle. To
reject stale `put`s from a `search()` that started before the mutation,
`RecentSearchCache` carries an `epoch: u64` counter that `invalidate`
bumps; `lookup` returns the current epoch on a miss and the runtime
threads it back into `put_if_epoch`, which refuses to publish results
pinned to an older epoch.

**Cache scope.** The cache is a daemon-internal optimisation. Direct DB
writes that bypass the daemon (`nagori --db <path>` against a shared
file while the daemon runs) cannot invalidate it, so the palette could
keep showing rows the CLI just deleted / unpinned until the next
mutation through the daemon. Use `--ipc` / `--auto-ipc` whenever the
daemon is alive.

**Ranker.** `DefaultRanker` combines weighted signals:

```text
score = exact_match + prefix + substring + fts + ngram
      + recency + frequency + pin + source_bonus
```

Pinned entries do not always dominate exact matches; recency is
saturated; use count is logarithmic; deleted and `Blocked` rows never
appear.

**CJK / Japanese.** Text is normalized with Unicode NFKC and lowercased
where applicable. CJK substrings are indexed as 2- and 3-grams capped at
`MAX_NGRAM_INPUT_CHARS` (4096) non-whitespace characters per entry,
which keeps a 512KB paste from exploding `ngrams` to ~1M rows. Queries
generate the same grams and rank by overlap before re-ranking with
exact substring + recency. In the Auto plan only CJK-bearing query grams
are used (see *Hybrid ngram is CJK-scoped* above); explicit `Fuzzy`
matches on the full gram set, which is what backs ASCII partial / typo
recall.

Two refinements live in the gram generator (not in `normalize_text`, so
the stored `normalized_text`, FTS index, previews, and semantic embedding
input are untouched). **Kana folding:** Katakana is folded to Hiragana
before gramming, so a Katakana clip recalls against a Hiragana query and
vice versa (`クリップ` ⇄ `くりっぷ`); the prolonged sound mark, middle dot,
and the rare `ヷヸヹヺ` pass through. **Han 1-grams:** documents also index
a 1-gram for each Han ideograph, so a lone-kanji query (`検`) recalls
entries beyond the bounded substring window — `unicode61` FTS collapses a
CJK run to one token, and the 2/3-gram path needs ≥ 2 chars. The 1-grams
stop at Han: kana 1-grams would be near-universal posting lists (`の`/`は`),
made worse by the fold. A 2+ char query keeps only its 2/3-grams so the
overlap denominator is unchanged.

**Ngram index versioning.** `search_documents.ngram_index_version` records
which generator revision built a row's grams; fresh captures stamp the
current revision in the same transaction as the grams. When the generator
changes (the kana fold / Han 1-grams above), pre-upgrade rows default to
`0` and a one-shot daemon worker (spawned after serving starts, so startup
never blocks) regenerates them in small batches from the stored
`normalized_text`, restamping each row and yielding the writer lock between
batches. CJK recall for not-yet-rebuilt rows is briefly incomplete and
self-heals as batches land. The direct/local CLI search path drains any
pending rebuild before querying (a single zero-row check once a daemon has
already run), so it never serves stale grams.

---

## 9. Sensitivity and redaction

**Detectors** (`nagori-core::policy`): API-key-like strings, JWTs, SSH
private keys (PEM blocks tolerate a missing END marker), AWS access
keys, GitHub tokens, Luhn-checked credit-card runs, OTP-like 6–8 digit
short codes, source-app denylist matches (typed identifiers from the
bundled password-manager preset plus free-text patterns — see
[`docs/privacy.md`](./docs/privacy.md#app-denylist)), and user-defined
regex.

**Classification output:**

```rust
SensitivityClassification {
    sensitivity: Sensitivity,
    reasons: Vec<SensitivityReason>,
    redacted_preview: Option<String>,
}
```

**Canonical scrubber.** `policy::redact_text` is the canonical text
scrubber and must keep parity with the detector list. In particular:

- PEM private-key blocks are matched as a span — including a missing
  END marker, since the detector flags as soon as `-----BEGIN` and
  `PRIVATE KEY-----` both appear.
- Credit-card candidates are 13–19 digit runs (with optional single
  spaces / dashes) gated by a Luhn check, so phone numbers and ISBNs
  are not touched.
- OTP redaction only fires when the **whole** trimmed body is a 6–8
  digit ASCII run, mirroring the classifier; arbitrary 6–8 digit
  substrings in prose are left intact.

**Settings-aware redaction.** `SensitivityClassifier::redact` wraps
`redact_text` and additionally applies the user's `regex_denylist`. Any
caller that needs settings-aware redaction (preview, AI input shaping,
`StoreRedacted` durable rewrite) must go through the classifier — not
the bare `redact_text` or the AI crate's `Redactor`.

**`SecretHandling` modes** (`nagori-core::settings::SecretHandling`):

- `StoreFull` keeps raw bytes recoverable from disk. Gated behind a
  confirmation in the desktop UI.
- `StoreRedacted` (default) rewrites the durable body, content hash,
  and FTS / ngram tokens to the redacted form **for new captures
  only**. Pre-existing rows, the SQLite freelist, and any backup still
  carry the raw bytes. Operators who need a clean DB should delete the
  affected rows and `VACUUM`; no in-place migration is provided.
- `Block` drops sensitive captures entirely (audited as
  `secret_blocked`).

---

## 10. Platform adapters

`nagori-platform` exposes traits only:

| Trait | Purpose |
|-------|---------|
| `ClipboardReader` | `current_snapshot()`, `current_sequence()`, bounded sequence/snapshot variants for the capture loop |
| `ClipboardWriter` | Restore an entry to the OS clipboard. `write_entry` / `write_plain` / `write_text` cover the primary-only contract; `write_representations` lets Preserve copy-back re-offer the publishable subset of captured MIMEs (text/plain, text/html, application/rtf, image/png, image/tiff, image/jpeg, image/gif, image/webp, text/uri-list) on adapters whose `clipboard_multi_representation_write` capability is `Available` (macOS — `clearContents` + `writeObjects` over an `NSPasteboardItem` batch under the arboard mutex, with inline reps sharing one item and each file URL fanning out to its own item so a multi-file list keeps every path; Linux Wayland — single-offer `copy::copy_multi` over `wlr_data_control` / `ext_data_control`; Windows — `OpenClipboard` + `EmptyClipboard` + N × `SetClipboardData` against `CF_UNICODETEXT` / `CF_HTML` / `Rich Text Format` / `CF_DIBV5` (plus a registered `"PNG"` companion) / `CF_HDROP` under the arboard mutex), with a default impl that falls back to `write_entry` on any adapter that does not advertise the capability. A companion `write_representation_exact` publishes exactly one chosen representation for the desktop "paste as <format>" picker: it validates the rep against the same per-adapter publishable table and, unlike `write_representations`, errors (`Unsupported`) instead of falling back to the primary, so the user never silently gets a different format than they picked. |
| `HotkeyManager` | Register / unregister palette and AI hotkeys |
| `PasteController` | Trigger Cmd+V / Ctrl+V into the frontmost app |
| `PermissionChecker` | Query / request Accessibility, Input Monitoring, Clipboard, Notifications, AutoLaunch |
| `WindowBehavior` | Frontmost-app metadata (bundle id, name, executable path) |

Implementations:

- **macOS** (`nagori-platform-macos`) — fully wired. `NSPasteboard`
  polling, `CGEventPost` for paste, `AXIsProcessTrusted` for
  Accessibility, frontmost-app metadata via the running-application
  list, and `open(1)` shelling for the `x-apple.systempreferences:`
  deep link.
- **Windows** (`nagori-platform-windows`) — daemon adapters are wired
  on top of `windows-sys` 0.61. The capture loop reads
  `GetClipboardSequenceNumber` plus arboard text (with a `CF_HDROP`
  file-list pass for file copies); auto-paste synthesises Ctrl+V via
  `SendInput`; frontmost-app metadata is collected through
  `GetForegroundWindow` + `QueryFullProcessImageNameW`. Preserve
  copy-back goes through `write_representations`, which pre-scans
  the stored rep set and then opens the clipboard once, calls
  `EmptyClipboard`, and walks a pre-allocated `(format, HGLOBAL)`
  list publishing `CF_UNICODETEXT` (NUL-terminated UTF-16), `CF_HTML`
  (with the `Version`/`StartHTML`/`EndHTML`/`StartFragment`/
  `EndFragment` header offsets), `Rich Text Format` (registered name,
  raw RTF bytes), `CF_DIBV5` (124-byte `BITMAPV5HEADER`, `BI_BITFIELDS`,
  BGRA, bottom-up, sRGB) with a registered `"PNG"` companion for
  apps that prefer the original PNG, and `CF_HDROP` (`DROPFILES` +
  wide-char path buffer) under the same arboard mutex used for text /
  image writes; handles whose `SetClipboardData` succeed are owned by
  the OS, while the failing handle and any not-yet-transferred
  successors are `GlobalFree`d so a partial transaction never leaks.
  Global hotkey registration is intentionally delegated to the Tauri
  shell's `global-shortcut` plugin (same MVP arrangement as macOS), so
  the daemon-side `HotkeyManager` reports `Unsupported`. Windows has
  no TCC-style user permissions for clipboard / synthetic input, so
  the `PermissionChecker` reports `Granted` for those kinds and
  `Unsupported` for `InputMonitoring`, `Notifications`, and
  `AutoLaunch` (managed elsewhere).
- **Linux** (`nagori-platform-linux`) — Wayland-only, wired for the
  daemon (`nagori daemon run` and `nagori-cli` in-process mode). The
  clipboard adapter talks directly to `wl-clipboard-rs` over the
  `wlr_data_control` / `ext_data_control` protocols; arboard is
  deliberately not used because its Linux backend silently falls back
  to X11 when the Wayland feature is missing or initialisation fails.
  Construction probes the data-control globals eagerly via
  `paste::get_mime_types` and refuses to start if neither protocol is
  exposed or no Wayland connection is reachable; `WAYLAND_DISPLAY` is
  the supported signalling channel because `wayland-client` consumes
  the inherited `WAYLAND_SOCKET` fd on first connect (the eager probe
  would burn it before the capture loop could reuse it). There is no
  X11 code path inside this crate. The capture path enumerates the
  offer's MIME types via `paste::get_mime_types` and reads each
  representation it cares about (image PNG/JPEG/GIF/WebP/TIFF in that
  priority order — mirroring the `nagori-core` factory allowlist —
  `text/uri-list` file lists, and text via the wl-clipboard-rs text
  fallback) through a shared SHA-256 hasher with per-rep MIME framing
  so the resulting sequence is unambiguous about the rep layout, not
  just the concatenated bodies. The cumulative hash also fixes the
  per-rep race window. Copy-back routes through `write_entry`, which
  publishes the matching MIME via `copy::MimeType::Specific` (selected
  with `image::guess_format` so the offer label matches the bytes) for
  image rows, serialises `text/uri-list` payloads from
  `ClipboardData::FilePaths` via `url::Url::from_file_path` (RFC 2483 —
  CRLF-separated `file://` URIs, absolute paths only), and falls back
  to the text rep otherwise. Preserve copy-back goes through
  `write_representations`, which pre-scans the stored rep set and then
  hands a single `copy::copy_multi` batch (text/plain, text/html,
  application/rtf, image/png|jpeg|gif|webp|tiff, text/uri-list) to the
  compositor so a paste target receives every advertised MIME alongside
  the plain-text fallback in one offer. Because Wayland exposes no equivalent of
  `GetClipboardSequenceNumber`, `current_sequence()` reuses the same
  multi-rep streaming hasher up to the configured byte ceiling;
  oversized transfers close the pipe immediately and use a
  ceiling/prefix-keyed sentinel sequence so the owner cannot hold a
  blocking worker by streaming past the limit. The source app participates in
  each transfer per the data-control protocol, so the capture interval
  (`AppSettings::poll_interval_ms`) directly trades off responsiveness
  against source-app wakeups. Auto-paste shells out
  to `wtype -M ctrl v -m ctrl`, which drives `zwp_virtual_keyboard_v1`;
  if the binary is missing or the compositor refuses the protocol the
  controller returns an error — the same shape as macOS when
  Accessibility is revoked. The clipboard write in `paste_entry` runs
  *before* the keystroke synthesis, so the entry is on the system
  clipboard regardless of the paste result and the user can complete
  the paste manually. `WindowBehavior::frontmost_app()` returns
  `Ok(None)` because Wayland has no portable frontmost-app query (the
  closest extensions — `zwlr_foreign_toplevel_management_v1`,
  `ext_foreign_toplevel_list_v1` — are compositor-specific). Hotkey
  registration on the daemon side is `Unsupported`. The Tauri desktop
  shell now wires the same `LinuxClipboard` + `LinuxPasteController` +
  `LinuxPermissionChecker` adapters through `AppState::build` and runs
  the in-process capture loop against them; a missing `wl_data_control`
  protocol surfaces at startup as an `AppError::Platform` with an
  explicit Wayland/X11 hint instead of silently degrading to a no-op
  runtime. The Tauri plugin surface — tray (via the StatusNotifierItem /
  `libayatana-appindicator` shipped in the deb dependency list),
  autostart (`~/.config/autostart/<bundle>.desktop`), global-shortcut
  registration, and updater — is wired on every OS; the global-shortcut
  backend is X11-only upstream, so pure Wayland sessions surface
  `nagori://hotkey_register_failed` and the UI falls back to the in-app
  open button.
  `PermissionChecker`
  reports `Granted` / `Denied` for `Clipboard` (probing the same
  `wl-clipboard-rs` entry point the capture loop uses) and
  `Accessibility` (probing `wtype --help` on PATH), and `Unsupported`
  for `InputMonitoring`, `Notifications`, and `AutoLaunch`. GNOME
  Wayland does not currently expose either data-control protocol; the
  error message points users at Sway, KDE Plasma 5.27+, Hyprland, or
  river.

**Permission model.** The platform layer exposes:

```rust
enum PermissionKind { Accessibility, InputMonitoring, Clipboard,
                       Notifications, AutoLaunch }

enum PermissionState { Granted, Denied, NotDetermined, Unsupported }
```

`PermissionChecker::check(&ctx)` returns the live state for every
kind. The context carries
`settings.onboarding.accessibilityPromptedAt` so the macOS adapter can
distinguish `NotDetermined` (we have never asked the OS to surface
the TCC dialog) from `Denied` (we have asked and `AXIsProcessTrusted`
still returns `false`). `PermissionStatus` also carries an optional
`reason_code` / `setup_route` / `docs_url` triplet that downstream
scripts and Setup cards branch on without parsing the message string.
The capture loop and copy paths only need `Clipboard`. The auto-paste
path needs `Accessibility`; when it is missing, the desktop and CLI
both fall back to **copy-only** behaviour (palette `Enter` and
`nagori paste` write to the clipboard but skip the Cmd+V synthesis).
The onboarding banner and `nagori doctor` surface the missing
permission so the user can fix it.

**Capability model.** Permissions answer "does this work right now";
capabilities answer "could this OS ever do it". The two are intentionally
separate surfaces:

```rust
struct PlatformCapabilities {
    platform: Platform, tier: SupportTier,
    capture_text: Capability, capture_image: Capability,
    capture_files: Capability, write_text: Capability,
    write_image: Capability,
    clipboard_multi_representation_write: Capability,
    auto_paste: Capability,
    global_hotkey: Capability, frontmost_app: Capability,
    permissions_ui: Capability, update_check: Capability,
    preview_quick_look: Capability, ai_actions: Capability,
}

enum Capability {
    Available,
    Unsupported { reason: String },
    RequiresPermission { permission: PermissionKind, message: String },
    RequiresExternalTool { tool: String, install_hint: Option<String> },
    Experimental { message: String },
}
```

`nagori_platform_native::capabilities()` aggregates per-OS reporters
(`nagori_platform_{macos,windows,linux}`) at build time and caches the
matrix on the `NagoriRuntime`. One row is reconciled rather than static:
`NagoriRuntimeBuilder` overwrites `ai_actions` from whether an `ai_engine`
is actually wired (`Available` if so, else `Unsupported`). That keeps the
matrix honest about a host's *real* AI backend — it is the single switch,
so a host that gains one (today macOS; a future cross-OS provider) lights
the desktop's AI surfaces up with no second edit, and a host with none
never advertises a backend it lacks. Live model readiness (Apple
Intelligence downloaded, etc.) stays on the separate `AiAvailabilityReport`
channel. The result is exposed on three surfaces:

- `runtime.capabilities()` — in-process access for the daemon and the
  Tauri shell.
- IPC `IpcRequest::Capabilities` → `IpcResponse::Capabilities(Box<…>)`
  — a control request that bypasses the `cli_ipc_enabled` gate so
  external diagnostics work even when scripted access is off.
- CLI `nagori capabilities` — in the default local path (no `--ipc` /
  `--auto-ipc`) the command short-circuits before the local DB open,
  so the probe still answers on machines with a misconfigured SQLite
  path. Honours `--json` / `--jsonl`.

The desktop shell additionally exposes the matrix as a `get_capabilities`
Tauri command, rendered read-only under Settings → Advanced. Wayland
`frontmost_app`, Windows `update_check`, and Linux Wayland `auto_paste`
without `wtype` are the canonical examples of where capability state
diverges from permission state — the UI can render an actionable hint
("install `wtype`", "switch to a wlroots compositor") instead of a
generic "feature unavailable" toast.

**Preserve copy-back hydration.** `ClipboardWriter::write_representations`
takes the stored representation set for the entry being re-copied and
replays the reps whose MIME has a known platform mapping in one
clipboard transaction. The macOS adapter
(`clipboard_multi_representation_write = Available`) publishes the
intersection of {`text/plain`, `text/html`, `application/rtf`,
`image/png`, `image/tiff`, `image/jpeg`, `image/gif`, `image/webp`,
`text/uri-list`} with the stored set. The reps are assembled into
`NSPasteboardItem`s off-pasteboard and then published in one
`clearContents` + `writeObjects` transaction: the inline reps (text /
HTML / RTF / image) share a single item — one value per type, set via
`setString:forType:` / `setData:forType:`, with JPEG/GIF/WebP declared
via dynamic UTIs (`public.jpeg` / `com.compuserve.gif` /
`org.webmproject.webp`) — while a `text/uri-list` rep fans out to one
item per file URL, the Apple-documented way to put multiple files on
the pasteboard. A multi-file copy-back therefore keeps every path
(Finder pastes all of them) instead of collapsing to the last URL the
way the earlier per-rep `setString_forType` loop on the implicit item
did. The `write_entry` FileList branch uses the same per-file
`NSPasteboardItem` batch (an empty path list is refused rather than
blanking the clipboard), so a primary-only copy-back of a file list
republishes as file URLs instead of pasting the paths as plain text.
Because the batch is built before `clearContents`, a rep set every item
of which AppKit rejects leaves the clipboard untouched. The Windows adapter
(`clipboard_multi_representation_write = Available`) builds every
`HGLOBAL` first — `CF_UNICODETEXT` for `text/plain`, `CF_HTML` (with
the documented `Version` / `StartHTML` / `EndHTML` / `StartFragment` /
`EndFragment` header) for `text/html`, the registered `Rich Text
Format` for `application/rtf`, `CF_DIBV5` (124-byte `BITMAPV5HEADER`
+ BGRA bottom-up pixels + sRGB colour space) for `image/png` (with a
registered `"PNG"` companion so apps that prefer the original byte
stream still get it) and for `image/jpeg` / `image/gif` / `image/webp`
/ `image/tiff` (decoded once via the `image` crate), and `CF_HDROP`
for `text/uri-list` — then opens the clipboard, calls `EmptyClipboard`,
and walks the handle list. Successful `SetClipboardData` calls transfer
ownership to the OS; on a mid-sequence failure the remaining
not-yet-transferred handles are `GlobalFree`d so a partial transaction
never leaks. The Linux Wayland adapter
(`clipboard_multi_representation_write = Available`) issues a single
`copy::copy_multi` over `wlr_data_control` / `ext_data_control` with
the same `MimeSource` set, so a Wayland paste target that prefers
`text/html` still sees it alongside the `text/plain` fallback. To
avoid clearing the clipboard for nothing, every adapter pre-scans the
rep set before touching the OS: when every entry falls outside the
publishable table the call falls back to `write_entry` *before* the
clear, so the existing primary-only path either restores the entry or
surfaces the same `AppError::Unsupported` it would have raised on a
direct `write_entry` call. Either way the previous clipboard contents
survive instead of being wiped out for a publish attempt that was
going to error anyway. Adapters whose capability is `Unsupported`
(`MemoryClipboard` in the daemon's in-process tests, plus the
fallback path used when no host adapter could be built) inherit the
default impl that delegates back to `write_entry`, so Preserve still
publishes the primary content there — just without the rich-MIME set.
The daemon-side wiring lives in `NagoriRuntime::copy_entry_with_format`:
the Preserve branch reads the persisted rows via
`EntryRepository::list_representations` and only falls back to
`write_entry` when the entry has no stored set (older history or
`add_text`-style synthesised rows). PlainText copy-back keeps its
existing `write_plain` path so plain-text-only targets always get the
plain fallback the capture pipeline normalised on insert.

**Paste as <format>.** A copied item that carries several representations
(a file that also offers a rendered image and a text label, say) can be
re-pasted as just one of them. `NagoriRuntime::list_paste_options` reads
the stored set and returns the distinct pasteable MIMEs (deduped, in
canonical role/ordinal order) via the shared `model::paste_option` helper,
and the desktop surfaces them from the alternate-format chord
(`Cmd/Ctrl+Shift+Enter`) as a small picker — shown only when there is a
real choice (≥2 distinct formats), otherwise the chord keeps its plain
alternate-format paste. Choosing one runs `copy_entry_representation`,
which re-reads the representation set (so a concurrent eviction can't make
the picker's snapshot stale), resolves the MIME to its single canonical row,
and publishes it through `write_representation_exact`. The whole chord
(picker rows and the direct fallback alike) is a deliberate paste, so it
runs with `PasteSynthesis::Force`: the ⌘/Ctrl+V keystroke fires even when
`auto_paste_enabled` is off (plain Enter still honours the setting), and a
synthesis failure surfaces the usual "copy succeeded — paste manually"
diagnostic rather than degrading silently to a copy. Sensitivity is unchanged
from Preserve — `Blocked` is refused and the chosen rep is a subset of what
Preserve already offers — and the wire contract stays desktop-local (the
IPC/CLI search result keeps its flat `preview`).

---

## 11. IPC boundary

**Transport.** Newline-delimited JSON over a per-platform stream
transport: Unix-domain sockets on macOS / Linux, Win32 named pipes on
Windows. The client writes one `IpcEnvelope { token, request:
IpcRequest }` line and reads one `IpcResponse` line. Defaults:

- **Unix.** Socket at
  `~/Library/Application Support/nagori/nagori.sock` (macOS) or the
  equivalent XDG location on Linux. The bind sets a tight umask so the
  socket inode is born `0o600`.
- **Windows.** Named pipe `\\.\pipe\nagori`. The first instance is
  bound synchronously during daemon startup with
  `ServerOptions::first_pipe_instance(true)` so a second daemon trying
  to publish the same name fails the launch (rather than only logging a
  warn line from a background task). The accept loop then chains fresh
  `NamedPipeServer` instances after each connect, mirroring the Unix
  `accept` semantics. The pipe is created with the default named-pipe
  security descriptor inherited from the daemon process — there is no
  custom DACL yet, so authentication relies on the sibling token file
  rather than on ACL filtering at the pipe level.

**Single-instance & stale-socket handling.** A daemon takes a
process-lifetime advisory lock (`nagori_storage::ProcessLock`, an
`flock(LOCK_EX)` / `LockFileEx` over a `nagori.lock` file) on the
**directory that holds its SQLite store**, acquired *before* it opens
the store (`acquire_data_dir_lock`, called from the CLI's `daemon run`)
and held for its whole run. The desktop shell locks the same directory
in `AppState::try_new_at`, so on **every** platform a second daemon — or
a daemon vs. the desktop app — against the same store refuses to start
rather than co-owning it and double-capturing. (Windows additionally
gets daemon-vs-daemon exclusion from the named pipe's
`first_pipe_instance(true)`, but the store-directory lock is the gate
that also covers the app.) The store lock is the authoritative
single-instance gate. `bind_unix` (the conservative primitive used by
`serve_unix` and tests) refuses any pre-existing socket; only a
lock-holding daemon's `bind_unix_replacing_stale` reclaims one, and it
does so only when the socket is **dead** (a `connect()` is refused) — a
crashed predecessor's, or its own after a supervisor restart. A socket
with a *live* listener (e.g. a daemon sharing this `--ipc` endpoint under
a *different* `--db`, whose distinct store lock did not exclude it, or a
non-nagori squatter) is refused rather than unlinked, so a live peer is
never left unreachable. Removal never hinges on a connect failure
*alone* — that failure mode is exactly what the lifetime lock was added
to close — it requires both the held store lock and a dead socket. The
kernel drops the lock on process exit — including a crash — so there is
no stale-lock file to clean up.

An auth-token file sits in the same directory as the IPC endpoint:
`nagori.token` next to the socket on Unix (`0600` mode set explicitly
during write), and under `dirs::data_local_dir()/nagori/` on Windows
(default NTFS permissions inherited from `fs::write`; no custom DACL).
When the user passes `--ipc <custom>` to either the daemon or the CLI,
both sides derive the token filename from the endpoint. On Unix the
derivation reuses the socket stem (e.g. `dev.token` for `…/dev.sock`).
On Windows the default pipe `\\.\pipe\nagori` keeps the historic
`nagori.token` filename; every other pipe is written as
`<sanitised>-<8 hex>.token` where the suffix is the first eight hex
characters of `SHA-256(pipe name)` — without it, two pipe names whose
sanitised tail collides (e.g. `\\.\pipe\a:b` and `\\.\pipe\a?b` both
sanitise to `a_b`) would race for the same token file. The server
rejects envelopes whose token does not match via constant-time
comparison.

`cli_ipc_enabled` is enforced live, not only at daemon startup. The
daemon supervises the IPC server from the settings watch channel: enabling
the toggle binds the endpoint and writes a fresh token, while disabling it
drains the accept loop and removes the socket/token files. The runtime
also rejects non-control IPC requests while the toggle is off; `Health`,
`Doctor`, `Capabilities`, and `Shutdown` remain available to support
diagnostics and orderly exit.

**Request / response types** (`nagori-ipc::protocol`):

```rust
enum IpcRequest {
    Search, GetEntry, ListRecent, ListPinned,
    AddEntry, CopyEntry, PasteEntry,
    DeleteEntry, PinEntry, Clear,
    RunAiAction,
    GetSettings, UpdateSettings,
    Doctor, Health, Capabilities, Shutdown,
}

enum IpcResponse {
    Search, Entry, Entries,
    AiOutput, Cleared,
    Doctor, Health, Capabilities,
    Ack, Error,
}
```

Permissions are not exposed over IPC; CLI clients query them through
`nagori doctor`, which is a separate request that aggregates version,
paths, daemon status, and `PermissionChecker::check()` output.

**Backpressure & limits.** `MAX_IPC_BYTES` caps per-message size. The
server uses an `accept_loop` over a `bind_unix` listener; each
connection runs on a tokio task and shares the same `NagoriRuntime`.
Beyond the read/write timeouts, the handler itself runs under a
`HANDLER_DEADLINE` backstop and a concurrent peer-disconnect watch: while a
handler runs the read half is watched for EOF, so when a client gives up and
closes (its own request timeout is far shorter than the deadline) the handler
future is dropped — cancelling its in-flight work and freeing the connection
permit, the AI permit, and any pending DB query — instead of finishing a
response no one will read. The deadline only fires for the degenerate case where
the peer neither reads nor closes while a handler is wedged; it is sized above
the longest legitimate handler (a `RunAiAction` bounded by its own absolute
deadline).

**Error model.** `IpcError` carries a stable `code` (English) plus a
human-readable `message`. The desktop frontend maps `code` to
localized copy; the CLI prints `message` directly.

See [`docs/ipc.md`](./docs/ipc.md) for the full request/response
schema.

---

## 12. Tauri boundary and frontend

**Command layer (`apps/desktop/src-tauri`).** Tauri commands are thin:
deserialize → call `NagoriRuntime` → serialize a DTO. Direct SQL,
ranking, or AI logic in a `#[tauri::command]` is a regression.

```rust
#[tauri::command]
async fn search_clipboard(
    state: tauri::State<'_, AppState>,
    query: String,
) -> Result<Vec<SearchResultDto>, CommandError> {
    state.runtime.search(query.into()).await.map_err(Into::into)
}
```

**Window / shell exception.** Two narrow categories of platform call
**do** live in the command layer because they coordinate desktop
focus, not domain logic:

- `state.window.activate_app(bundle_id)` (via the
  `WindowBehavior` trait) — reactivates the app that was frontmost
  before the palette stole focus, so Cmd+V lands in the right window.
  The palette-confirm path runs this **regardless of the `auto_paste`
  setting**: with auto-paste on, focus must return before the synthesised
  Cmd+V; with auto-paste off, plain Enter leaves the user to paste manually,
  and restoring focus means their next Cmd+V hits the source window without
  first clicking to re-activate it. (The explicit "paste as" chord forces the
  synthesised Cmd+V even when auto-paste is off — see the paste-as section in
  §10 — so focus restoration is required there too.) (Linux Wayland captures no
  frontmost handle, so this is a no-op there and the compositor's own post-hide
  focus handoff returns focus to the source surface.)
- `request_accessibility(prompt: bool) -> PermissionStatus` — on macOS
  calls `AXIsProcessTrustedWithOptions(kAXTrustedCheckOptionPrompt:
  prompt)`. When `prompt = true` the runtime stamps
  `settings.onboarding.accessibilityPromptedAt`. The command also
  falls back to `open(1)` with the `x-apple.systempreferences:` URL
  when `prompt = true && !Granted && previously_prompted` — i.e. the
  user has clicked through the TCC dialog before but is still not
  trusted, so the OS dialog is suppressed and the Privacy pane is the
  next actionable step. On the *first* prompt we deliberately skip the
  `open` fallback so the OS dialog stays foregrounded; once the marker
  is set, subsequent clicks of the Setup card route to System Settings
  directly. Windows/Linux return a synthetic Granted (UIPI caveat) /
  wtype-presence row.

Both are deliberately scoped to UI focus / shell integration and do
not duplicate runtime logic.

**Frontend layout** (`apps/desktop/src/app/components/`):

- `Palette.svelte` — top-level container. Stacks `SearchBox` →
  `FilterChips` → (`ResultList` + `PreviewPane`) → `StatusBar`. The body's
  right column is shared: `ActionInspector.svelte` takes it over the preview
  pane while the action inspector is open (and forces an expanded full-width
  preview back to the list+panel split first).
- `FilterChips.svelte` — single-line quick-filter row directly under the
  search input. Composite filters that compose freely, split by cardinality:
  the low-churn axes stay as one-click chips — a single-select date window
  (*Today* / *Yesterday* / *Last 7 days* / *Last 30 days*) and a *Pinned*
  toggle — while the high-cardinality axes collapse into `FilterDropdown`
  menus so the row never wraps: multi-select content kinds (*Text* / *URL* /
  *Code* / *Image* / *Files*, each mapping to one `ContentKind`) and a
  single-select source app. The source-app options are retained from the last
  search that was *not* itself source-app-filtered (`recordSourceApps`, capped)
  rather than read from the live results — otherwise selecting an app would
  collapse the results, and the menu, to that one app and hide the others; this
  way the open menu keeps offering every app to switch to. A leading *All apps*
  row clears the selection, so the single-select axis has a discoverable reset
  instead of an obscure re-click. Each dropdown folds its selection into the
  trigger label (none → axis name, one → that value, many → `<axis> <n>`,
  avoiding per-locale plural forms). A *Clear* control appears only while some
  filter is active. All feed `currentFilters()` into every `searchClipboard`
  call; the daemon's search-cache key compares the full `SearchFilters`
  struct, so each combination caches independently. Re-clicking the active
  date chip clears it.
- `FilterDropdown.svelte` — reusable popover used by `FilterChips` for the
  content-kind and source-app axes. A trigger button (`aria-haspopup="menu"`)
  opens an opaque `--bg-overlay` menu of `menuitemcheckbox` (multi) or
  `menuitemradio` (single) rows. It owns its own keyboard handling and stops
  those keydowns from bubbling — the palette routes arrows / Enter / Escape at
  the window level, so an un-stopped menu keystroke would otherwise move the
  result selection or dismiss the palette (mirrors `ActionInspector`). Escape
  closes only the menu; a click outside dismisses it.
- `StatusBar.svelte` — entry count, last-search elapsed time, capture
  badge, AI badge, keyboard hints. Also hosts a one-row Accessibility
  indicator: when the OS grant that auto-paste needs is missing it
  surfaces a warning plus a *Setup* CTA that opens the Settings window
  on the Setup tab (`open_settings` with a `route` hint →
  `nagori://navigate`). The row resolves the shared 5-state
  `resolvePermissionUiState`, so it hides once the grant lands and on
  `Unavailable` platforms (Windows, Wayland sans `wtype`) where there is
  nothing to chase. It replaces the former `OnboardingBanner` card. It
  also hosts a persistent auto-paste **diagnostic chip** driven by the
  `pasteDiagnostics` store: when a paste fails, the chip carries the
  localized per-reason remediation in its `title` and outlives the toast
  (priority paste-diagnostic > accessibility warning > hints; an
  `accessibilityMissing` reason folds into the accessibility chip so the
  two never stack). Clicking it dismisses the diagnostic.
- `ResultItem.svelte` — kind-aware row renderer. URL rows emphasise the
  domain and add a strong-brand badge (GitHub / YouTube / …) derived from
  the hostname alone (`lib/urlCategory`, no network). Code rows show a
  language badge sourced from the backend `language` (the same canonical
  id the preview highlighter uses), falling back to a client-side sniff
  (`lib/codeLanguage`) only for legacy rows that predate detection. Image
  rows — which carry no body text — surface the probed `width×height`
  dimensions, the primary payload's byte size, and a *Screenshot* badge
  when the source app looks like a screenshot tool (`lib/screenshotSource`).
  A small reason chip surfaces the strongest *match* signal (*Exact* /
  *Prefix* / *Match* / *Text* / *Fuzzy* / *Semantic*) for query-driven
  rows; recent-listing rows stay chip-free since their only reason is
  recency. Semantic / fuzzy hits get a distinct hue so they read as a
  deliberate match type rather than a weaker one. The row's preview text
  is run through the shared `lib/highlightQuery` helper — a
  case-insensitive raw-substring scan (one pass per whitespace term,
  overlapping ranges merged) — so exact / substring / CJK hits are marked
  in place via `HighlightedText.svelte`; FTS / semantic hits that have no
  literal substring simply show no marks and lean on the reason chip. The
  query is `searchState.appliedQuery` (the query the visible results were
  produced for), threaded through `ResultList`, and the same helper marks
  the preview pane body, so list and preview stay in lockstep. Rows carry
  `content-visibility: auto` + `contain-intrinsic-size` so off-screen
  rows skip layout/paint: with the search limit at 50 (palette row cap
  64) this keeps arrow-key navigation cheap **without** the keyboard-nav /
  `scrollIntoView` / hover-selection regression risk that true windowing
  would carry against `ResultList`'s carefully-tuned scroll effect. If a
  future surface raises the result limit into the hundreds, revisit
  windowing then; the row-level containment is the low-risk first step.
- `PreviewPane.svelte` — hydrates full preview lazily through
  `get_entry_preview` (head+tail-truncated at 128 KiB / 4 000 lines so the
  end of large bodies stays visible). Includes a token-based syntax
  highlighter for `code` kinds; non-code bodies (text / richText /
  unknown) instead run through the shared `lib/highlightQuery` helper so
  the same query match the result row marks is visible in the full body
  (the helper caps its own scan at 32 KiB so a large body stays bounded).
  When a search query matches text
  inside the elided middle, the DTO's `elidedContainsMatch` flag surfaces a
  warning. For Public text entries the pane offers an "expand" button that
  fetches the body up to 1 MiB via `get_entry_preview_full`; non-Public
  entries hide the affordance because the IPC enforces the same gate.
  URL entries use a dedicated three-tier layout (`host_display` on top,
  `scheme` + `path_and_query` muted below) sourced from the
  `PreviewBodyDto::Url` fields populated via `url::Url::parse` +
  `idna::domain_to_unicode`. When the displayed Unicode host differs from
  its ASCII (`xn--…`) form a `role="status"` punycode badge surfaces the
  ASCII via `title`. The pane registers an Enter-key handler in
  `expanded` mode that opens a confirm modal and only then invokes
  `open_url_external(entry_id, url)`. The backend command re-fetches the
  entry, verifies the URL matches the stored claim, enforces
  `Sensitivity::Public`, restricts the scheme allowlist to `https` /
  `http`, and dispatches via `open --` (macOS), `ShellExecuteW` (Windows
  — avoids the `cmd.exe` argument parser), or `xdg-open` (Linux).
  Non-Public entries hide both the Enter hint and the open button. The
  full-width expanded preview is toggled by the `open-preview` binding
  (default `CmdOrCtrl+E`, remappable via `paletteHotkeys`, also reachable
  from the status-bar **Preview** hint button); the status bar surfaces the
  resolved accelerator so the feature is discoverable rather than buried.
  Image kinds render through `PreviewBodyImage.svelte`, whose summary chip
  shows `dimensions · format · size`; in the expanded preview it zooms the
  original payload. The primary gestures are pointer-driven — a trackpad
  pinch (WebKit delivers this as the non-standard `gesturechange` event with
  a cumulative `event.scale`, since wry leaves WKWebView's native
  magnification off), `Ctrl`/`Cmd` + wheel (the cross-platform / Chromium
  pinch path), and double-click to toggle fit ↔ 2×. A keyboard chord
  (`CmdOrCtrl` + `=` / `+` in, `-` out, `0` refit) is the secondary,
  keyboard-first path — a *chord* rather than a bare key because the search
  box keeps focus across the palette, so a bare `0` / `-` would be an
  ordinary search character, whereas a modifier chord types nothing into the
  field and lets the `window` listener stay correct no matter where focus
  sits (Tauri ships with webview zoom hotkeys disabled, so the chord reaches
  the app instead of resizing the whole UI). Each gesture anchors on its own
  point: after the zoom the scroll offset is re-pinned so the pixel under the
  pointer (or the frame centre, for the keyboard chord) stays put, rather than
  growing the image off the top-left corner. Zoom sizes a scroll *stage* in
  CSS rather than applying a `transform`, so the frame's `overflow: auto`
  becomes real scroll-to-pan once the image is larger than the pane; the stage
  scales by the *continuous* (unrounded) zoom so its size matches the
  re-pin ratio exactly — a sub-percent step that left the stage put would
  otherwise drift the anchored point. The frame sets `user-select: none` so the
  double-click toggle never leaves an OS text/range selection behind
  (`dblclick`'s `preventDefault` can't undo a selection the preceding mousedown
  already made). A small percentage readout (a `role="status"` live region,
  rounded to a whole percent) stays visible the whole time the preview is
  expanded — including at 100 %, where it doubles as a hint that the image is
  zoomable.
  `fileList` bodies render through `PreviewBodyFileList.svelte` from the
  backend-supplied `FileEntry[]` (basename, home-folded parent, extension,
  kind) plus a hoisted `commonParentDisplay`: a single file becomes a
  basename heading over a `Location` row, while multiple files share the
  common-parent header with each row dimmed relative to it. The renderer no
  longer splits raw paths — the `nagori-core` `file_path` module does that
  server-side (basename / parent / longest-common-parent / `~` folding /
  extension), the same width-independent rules `lib/filePath.ts` keeps for
  the palette badge and colour dot.
  The footer keeps the resting identification aids visible — the *source*
  line, and *additional clipboard data* when present: the coarse, user-facing
  categories (Image / Text / Files) a clip kept *beyond* its primary kind,
  rather than the raw MIME list (result rows no longer carry that chip at all,
  as it was an internal-format dump with little bearing on selection). That
  row is informational, not a paste-format picker affordance — the ⇧⌘⏎ picker
  opens only for ≥2 pasteable formats. The header carries a resting privacy
  badge for `Secret` / `Blocked` entries, mirroring the row chip; its absence
  is deliberately not a "Public" claim. The remaining technical fields — id,
  sensitivity (the full value, every entry), size, and *rank* (the entry's
  `RankReason`s as localised labels, the same vocabulary as the row chip, so
  the full "why it matched / why it ranked here" set including the recency /
  frequency / pin boosts the row chip omits is recoverable) — fold into a
  collapsed **Details** disclosure so the resting pane leads with the body
  rather than diagnostics.
- `ActionInspector.svelte` — a hotkey-triggered **docked panel** that runs
  actions against the selected entry. It is not a modal: opening it (the
  `open-actions` binding) takes the palette body's right column in place of
  `PreviewPane.svelte` and hands it back the moment it closes, so the target
  stays beside its source row and long results read against full panel height
  rather than the bottom of an 80vh overlay. While it is open the result list
  becomes a read-only reference surface — the target row lifts and the rest
  recede, and hover, row clicks, and the per-row pin button are all inert, so a
  stray mouse move or click can't silently re-target (and cancel a running
  action or discard a finished result) or tear down the palette; ↑/↓ stay the
  deliberate re-target path and Escape returns to live browsing. It composes
  `CompactPreview.svelte` (the target's kind / source / time and a short
  snippet, kept visible so the user always sees what they are acting on),
  `ActionPicker.svelte` (one flat list mixing the deterministic quick actions
  — Summarize first sentence, Format JSON, Extract tasks, Redact secrets —
  with the availability-gated, model-backed AI actions — *Summarize*,
  *Rewrite*, *Format as Markdown*, *Organize tasks*, *Explain code* — each
  marked with a small `AI` badge instead of a label prefix), and
  `ActionRunPanel.svelte` (a single `idle / running / result / error` work
  area that grows to fill the panel). Streaming AI partials and the final
  result land in that one area, so the output never jumps position on
  completion; the AI actions stream over the `nagori://ai/*` events, and a
  fast deterministic run skips the running indicator (shown only once it
  outlives ~120 ms). Each AI button is disabled with a remediation tooltip
  when its action is unavailable. The panel is a focusable non-modal
  `role="dialog"` that stops keydowns from leaking into the palette's
  window handler while focused. Escape cancels an in-flight stream and
  otherwise closes the panel; pressing the `open-actions` chord again
  toggles it shut (cancelling any run, unlike Escape's cancel-then-stay).
  Opening is not keyboard-only: the *Actions* button in the preview-pane
  header and the clickable ⌘K hint in the status bar both call the same
  open path, so the entry gesture matches the mouse-driven action
  selection. The result shows *Copy* (uses
  `navigator.clipboard`) and *Save as new entry* (calls `save_ai_result`).
  Clearing the whole history is not offered here — that global, destructive
  action lives on the tray menu and the `clear-history` hotkey.
- **Quick Look (macOS only).** Cmd+Y on a selected palette row invokes
  the `preview_entry` Tauri command, which is gated to the desktop
  process because the daemon does not host an AppKit event loop. The
  command rejects non-`Public` entries up front, then materialises the
  entry payload under `std::env::temp_dir()/nagori-preview/<entry_id>.<ext>`
  through `nagori_storage::ensure_private_directory` (restrictive perms)
  and spawns `/usr/bin/qlmanage -p` via the `PreviewController` adapter
  (`MacosPreviewController`). `qlmanage` was chosen over an in-process
  `QLPreviewPanel` to avoid the ObjC data-source protocol — the on-screen
  affordance is identical. Windows and Linux Wayland report
  `preview_quick_look = Unsupported` (Windows has no OS-provided
  overlay; Linux lacks a desktop-environment-agnostic equivalent), and
  the palette gates the keybinding on the capability snapshot so the
  shortcut becomes inert outside macOS. The `<entry_id>.<ext>` temp files
  are a plaintext cache, so they are scrubbed on every history-erasure
  surface rather than left to accumulate: the desktop `setup` hook wipes
  the whole `nagori-preview/` dir at launch (clearing a crashed session's
  leftovers), `delete_entry` / `delete_entries` remove the matching
  `<entry_id>.*` file, `clear_history` purges the whole dir, and the
  `clear_on_quit` exit path purges it again. A regenerate-on-demand cache
  means a pinned entry's temp file removed by `clear_history` is simply
  rebuilt on its next preview.
- `SettingsView.svelte` — tabbed *Setup* / *General* / *Privacy* / *CLI* /
  *Advanced* settings panel. The Setup tab mounts `SetupRoute`, which
  hosts a `PermissionCard` per OS permission and is selected by default
  on first launch (when both `onboarding.completedAt` and
  `onboarding.accessibilityFirstGrantedAt` are still `null`) so the user
  lands directly on the permission grant flow. On the same first-launch
  condition the backend surfaces the Settings window itself
  (`surface_first_launch_setup` in `lib.rs` calls `show_settings_window`
  during `setup()`) so the user does not have to discover the StatusBar
  indicator on their own; daemon and hotkey registration keep running
  regardless. Denylists are edited as
  multi-line textareas serialised back into `string[]`; capture kinds,
  paste format, recent ordering, total storage limit, and appearance
  are exposed as structured controls.
- `PermissionCard.svelte` + `lib/permissions.ts` — frontend surface for
  the OS permission flow. The card derives a 5-value
  `PermissionUiState` (`NotRequested` / `PromptShownNotGranted` /
  `Granted` / `RevokedAfterGranted` / `Unavailable`) from the
  `PermissionStatus` snapshot, the `OnboardingSettings` sticky markers,
  and the platform tag, then routes the Grant CTA through
  `request_accessibility(prompt=true)`. A module-scoped refcounted
  poller in `lib/permissions.ts` re-fetches `getSettings` +
  `getPermissions` every 2 s while at least one card is mounted, pauses
  on `visibilitychange: hidden` / `window#blur`, does a one-shot fetch
  on `window#focus`, and emits a `'timeout'` event after 60 s of
  ungranted polling so the card can render an inline "Re-check" error.
  The timeout only caps the wait for the *first* grant: once a poll has
  observed `accessibilityGranted()`, the poller keeps ticking
  indefinitely (still bounded by the visibility pause) so a later revoke
  surfaces while the Setup tab stays in focus.

The `stores/settings.svelte.ts` store is the single source for
`captureEnabled()`, `accessibilityState()`, and
`accessibilityGranted()`; status bar and onboarding banner subscribe
to it.

**DTOs** (`apps/desktop/src-tauri/src/dto.rs`):

```ts
type SearchResultDto = {
  id: string;
  kind: "text" | "url" | "code" | "image" | "fileList" | "richText" | "unknown";
  preview: string;
  score: number;
  createdAt: string;
  sourceAppName?: string;
  pinned: boolean;
  sensitivity: "Unknown" | "Public" | "Private" | "Secret" | "Blocked";
  rankReasons: string[];
  // Basename-first projection for fileList rows so the palette leads with
  // filenames + a home-folded location instead of a shared absolute prefix.
  // Hydrated from the canonical paths only for Public/Unknown rows; absent for
  // other kinds and for sensitive file lists (those fall back to `preview`).
  fileSummary?: {
    total: number;
    representativeNames: string[];
    commonParentDisplay?: string;
    locationCount?: number;
  };
};

type EntryDto = {
  id: string; kind: string; text?: string;
  preview: string; createdAt: string; updatedAt: string;
  lastUsedAt?: string; useCount: number; pinned: boolean;
  sourceAppName?: string; sensitivity: string;
};
```

**Sanitized errors.** `CommandError { code, message, recoverable }` is
the only error shape the frontend sees. File system paths, raw OS
errors, and clipboard content never cross this boundary.

---

## 13. CLI

`nagori-cli` should stay **thin**: argument parsing, output formatting,
and a choice between IPC client and direct DB access. Capture, search,
and AI logic stay in `nagori-daemon` / `nagori-core`.

**Modes.**

- `--ipc <socket>` — talk to a running daemon.
- `--auto-ipc` — probe the default socket and fall back to direct DB
  access when the daemon is not running.
- `--db <path>` — repair / offline mode. Bypasses the daemon and
  cannot invalidate its in-memory search cache; documented as such.

**Output formats.**

- Plain text (default) for humans.
- `--json` — single document per command.
- `--jsonl` — one record per line for streaming and agent pipelines.
- CLI surface stays English-only on purpose (see
  [section 15](#15-internationalization)).

**Stable exit codes.** `0` success, `2` invalid input, `4` not found,
`5` policy denied, `6` permission denied, `7` unsupported, `8`
internal error. Agents and scripts can branch on these without parsing
text. See [`docs/cli.md`](./docs/cli.md) for the full command list.

**Doctor.** `nagori doctor` prints version, paths, daemon status,
permission states (Accessibility, Input Monitoring, Notifications,
AutoLaunch), and three background-health rows: `maintenance` for the
retention loop, `capture` for the steady-state capture loop, and
`startup` for the capture loop's pre-poll initialisation. The
canonical first step for support tickets.

**Startup health.** `NagoriRuntime::startup_health()` returns a
shared `StartupHealth` snapshot. The host process (`serve.rs` for the
daemon, `state.rs::spawn_background_tasks` for the desktop) records
either `record_capture_ready()` once settings load and the capture
loop is entering polling, or `record_capture_failed(reason)` on the
silent-abort path that used to leave the user staring at a "Clipboard
history is ready." notification while capture quietly never started.
The snapshot is sticky after the first outcome, so transient
re-inits cannot mask the original failure — both `nagori doctor` and
the desktop's gated startup notification read the same signal.

**Capture health.** `NagoriRuntime::capture_health()` returns a shared
`CaptureHealth` snapshot covering the capture loop's *steady-state*
polling (where `StartupHealth` covers one-shot pre-poll init). The
loop posts `record_success` on every healthy tick, `record_error` with
an `Adapter` / `SettingsLoad` category on reader / classifier
failures, and `record_drop` with a `Policy` / `OversizedDrop` category
on intentional refusals. The failure counter and the drop category
are tracked separately so a burst of policy drops cannot mask a real
adapter outage; once the counter crosses
`CAPTURE_DEGRADED_THRESHOLD`, both `nagori doctor` and the desktop
tray tooltip flip to degraded with the recorded category, and the
startup-ready notification body switches to "Nagori is running, but
clipboard capture is currently degraded." instead of falsely claiming
readiness.

**Distribution.** The `nagori` binary is **not shipped as a separate
download** — it rides inside the desktop bundle as a Tauri
`bundle.externalBin` sidecar so there is a single artifact to install
and version. `scripts/build-cli-sidecar.mjs` (run from
`beforeBuildCommand`) compiles `nagori-cli` for the target triple and
copies it to `apps/desktop/src-tauri/binaries/nagori-<triple>`; Tauri
strips the triple and lands it at `Contents/MacOS/nagori` inside the
`.app`. Because `tauri-build`'s `build.rs` validates `externalBin`
existence on *every* `cargo` compile, the `externalBin` key lives in a
bundle-only `tauri.bundle.conf.json` that is merged via
`tauri build --config` (the `desktop-build` Makefile target and
`release.yaml`) rather than the base `tauri.conf.json` — plain `cargo
check` / CI jobs that never build the sidecar stay green.

Getting that bundled binary onto a user's PATH has two paths. The
Homebrew cask emits a `binary "#{appdir}/Nagori.app/Contents/MacOS/nagori"`
stanza (see `.github/workflows/brew-bump.yaml`) so a cask install links
it automatically. For direct `.dmg` installs — where Finder-launched
apps inherit only launchd's minimal PATH — the desktop exposes an
in-app installer (`commands::install_cli` /
`commands::cli_install_status`, surfaced in **Settings → CLI**) that
symlinks the sidecar into `~/.local/bin` without an admin prompt. The
status command probes the user's *login + interactive* shell PATH (via
`$SHELL -lic`) rather than the GUI process environment, so the UI can
tell the user when `~/.local/bin` still needs adding to PATH.

---

## 14. Quick actions and AI actions

Two distinct, type-separated families:

- **Quick actions** (`QuickActionId`: `FormatJson`, `ExtractTasks`,
  `RedactSecrets`, `SummarizeFirstSentence`) are deterministic on-device
  transforms run by `nagori-ai`'s `QuickActionRunner`. They never touch a
  language model and are always available, independent of the AI provider
  configuration. `NagoriRuntime::run_quick_action` shapes the input through the
  settings-aware redaction classifier and the per-action `max_bytes` cap before
  the runner sees it. Both action families operate on the entry's text
  representation, so content with none (an image) is refused with `InvalidInput`
  rather than run on an empty string — `shape_ai_input` and the quick-action path
  share the same `actionable_text` guard.
- **AI actions** (`AiActionId`: `Summarize`, `Translate`, `Rewrite`,
  `FormatMarkdown`, `ExtractTasks`, `ExplainCode`) are model-backed and resolved
  through the `AiActionEngine`. On macOS the text-generation actions
  (`Summarize`, `Rewrite`, `FormatMarkdown`, `ExtractTasks`, `ExplainCode`) run
  on Apple's `TextGenerator` and `Translate` on the Apple `Translator`; on other
  platforms no engine is wired, so every AI action reports a capability mismatch.
  The text-generation actions differ only by their system prompt; `ExtractTasks`
  prompts for a Markdown checklist (guided generation via Apple's `@Generable`
  needs the macro compiler plugin, which ships only with full Xcode, not the
  Command Line Tools the bridge builds against). Each prompt closes with an
  explicit output-language directive naming the UI-language setting
  (`AiRequestOptions::output_language`, filled by the daemon from
  `settings.locale`, with the `system` sentinel resolved to the OS language);
  an indirect "keep the original language" hint does not hold the on-device
  model, which then defaults to English even on non-English input, so every
  action names the target language instead. The trade-off is that input in a
  different language than the setting is translated rather than preserved —
  reliably keeping a foreign input's language would need per-input detection
  the bridge does not expose.

**Engine layering.** `nagori-ai` is provider-agnostic. `AiEngine` resolves an
action to a backend family via the static `(action, provider) → backend` table,
then dispatches to a `TextGenerator` / `Translator` / `Embedder` backend. The
Apple backends (`nagori-ai-apple::AppleFoundationBackend` for text generation,
`AppleTranslateBackend` for translation, `AppleEmbedderBackend` for embeddings)
are injected by `nagori-platform-native` on macOS; other platforms wire no
engine, so AI actions are refused there while quick actions keep working.

**Desktop gating.** Refusal is the backstop, not the user-facing story: the
desktop hides every AI surface where no engine is wired rather than offering
controls that can only fail. The wired-engine fact reaches the frontend as the
`ai_actions` capability (see §11), and `aiActionsSupported()` gates on it — the
Settings *AI* tab and the action menu's AI actions disappear on a backendless
host. Because the gate reads the capability, not a hardcoded platform, a host
that wires an engine later restores those surfaces automatically. Actions are
also gated by the focused entry's `ContentKind`: `ActionInspector` disables (with
a reason hint) every action on an image or file list, and on a bare URL all but
`RedactSecrets` — the one transform that is meaningful on a token-bearing URL.
This is a UX layer over the daemon's `actionable_text` refusal, not the safety
boundary. The Settings
*Setup* tab is gated independently on whether its lone prerequisite needs
action (`auto_paste` ∈ {`RequiresPermission`, `RequiresExternalTool`}): shown
for the macOS Accessibility grant and the Linux `wtype` helper, hidden on
Windows where auto-paste just works.

**Translation.** `AppleTranslateBackend` wraps the `Translation` framework's
`TranslationSession`, detecting the source language with `NLLanguageRecognizer`
when the caller does not pass one and translating into the requested target
(`AiRequestOptions::target_language`). Translation is one-shot, so the engine
adapts the single `TranslationOutput` into a terminal `Done` event; a missing
language pack surfaces as an `AssetMissing` error carrying a download
remediation. `nagori ai translate <id> --to <lang> [--from <lang>]` drives it
from the CLI. The framework requires the app-bundle runtime context, so live
translation is exercised in the desktop app (Nightly), not the headless CLI.

**Semantic search.** A separate, opt-in toggle (`ai.semantic_index_enabled`,
distinct from the AI master switch) builds an on-device embedding index so
search can match by meaning. `AppleEmbedderBackend` wraps `NLContextualEmbedding`
via the Swift bridge, mean-pooling per-token vectors into one L2-normalised
document vector; it is pinned to one language/model (the user's preferred
language) so every stored vector shares a comparable embedding space. The
embedder's runtime metadata (`model_identifier` / `revision` / `dimension` /
`max_sequence_length`) is persisted alongside the vectors; a mismatch (model,
revision, or dimension change) clears the index and rebuilds it rather than
mixing incompatible spaces. Vectors live in `nagori-storage` as little-endian
float32 BLOBs ranked by `sqlite-vec`'s `vec_distance_cosine` (a Cargo feature so
the native extension stays optional). A background worker
(`nagori-daemon::semantic_index`) embeds freshly-captured clips and backfills
history in bounded batches, guarded by a battery check (AC-only by default), the
embedding concurrency permit, and rate-limit backoff; the settings UI exposes a
*Rebuild index* control plus live progress. The index is sensitivity-aware: the
`semantic_pending` query excludes `Secret` entries outright (so even a
`StoreFull` secret's raw body never reaches the embedding model), and the worker
runs every `Private` body through the settings-aware redactor
(`SensitivityClassifier::redact`) before embedding so private content is never
sent verbatim; `Public` / `Unknown` bodies embed as-is. This shaping is recorded
in `INDEX_VERSION` — bumping it (it was raised when this gate landed) is treated
like a model change, so the worker clears any vectors built under the old shaping
and rebuilds. At query time `SearchMode::Semantic`
embeds the query and ranks the stored vectors; the embedder is macOS-only, so on
other platforms (or when the model is unavailable) semantic search reports
`Unsupported` and the text plans keep working.

**Effective policy.** `NagoriRuntime::start_ai_action` builds one
`EffectiveAiPolicy` up front by tightening the per-request `AiRequestOptions`
against the `AiSettings` (and the model's hard input cap): the timeout is
`min(settings, request)`, the input-token cap is the model cap lowered by any
override, the output-token cap is the request's value (no settings counterpart),
and streaming is `allow_streaming && request`. That single value then drives the
input-shaping guard, the deadline, the options stamped onto the request handed
to the backend, and the stream wrapper — so a "tightening only" override is
actually applied rather than documented and ignored, and no limit is re-derived
(and drifts) between those sites. Timeout, the input-token cap, and streaming
are enforced daemon-side before the backend runs; the output-token cap is
*forwarded* (output length is only knowable mid-generation), so a backend caps
on it where it supports a max-output control — the on-device Apple generator
does not yet, so that value is carried but not honoured there.

**Streaming + cancellation.** `NagoriRuntime::start_ai_action` gates on the
`ai.enabled` master toggle, the allow-list, and the selected provider; shapes
the input (redaction, byte cap, and the effective token budget — a ~3,500-token
model cap, tightened by any per-request `max_input_tokens` — that refuses
oversized input rather than letting the model silently truncate); acquires the
backend's concurrency permit (text generation is serialised to one, matching
Apple's single-request model); and registers the request in the
`AiRequestRegistry`, which owns the `CancellationToken`. An **absolute
deadline** is anchored at registration (`now + effective timeout`) and bounds
*every* phase — the permit wait, `engine.start`, and the streamed generation all
draw down the same budget — so a wedged predecessor holding the single
text-generation permit, or a stalled `engine.start`, can no longer keep a
request (and its permit) alive past the configured timeout; previously the
timeout armed only after start. When streaming is not allowed (the
`allow_streaming` UI toggle off, or the request opting out) the daemon suppresses
intermediate `Delta` / `Replace` snapshots server-side and surfaces only the
terminal result, so the toggle holds for every surface regardless of whether the
backend itself streams. The returned stream of `AiEvent`s
(`Delta` / `Replace` / `Done` / `Cancelled`, with errors as `Err(AiError)`
items) releases the permit and removes the registry entry — and cancels the run
— when dropped. That cleanup is poll-driven, so a dedicated **AI watchdog**
(`run_ai_request_watchdog`, supervised in both the daemon and the desktop)
sweeps the registry every minute and reaps any handle still alive past its
absolute deadline — the polling-independent backstop for a stream that is
returned but never polled or dropped. Reaping on each request's own deadline
(rather than a flat TTL riding on the 30-minute maintenance cadence) reclaims a
short request's permit promptly while leaving a long-but-legitimate one alone
for its full budget. The desktop drives the engine in-process and re-emits events on
the request-scoped `nagori://ai/*` Tauri channel (coalesced); it also waits for
those listeners to attach before starting a run, so a fast terminal event can't
fire before the renderer is listening and strand the inspector in the running
state. The CLI's `nagori ai` streams in-process to stdout (`Ctrl-C` cancels).
The IPC `RunAiAction` envelope carries the same `AiRequestOptions` (so a CLI
`ai translate --from/--to` keeps its languages over the wire), drives the engine
to completion, and returns a single `AiOutput`; `RunQuickAction` runs the
one-shot quick path.

**Privacy contract.** Apple's on-device models run fully local
(`AiInputPolicy::allow_remote` is always `false`). The input-policy pipeline
(`require_redaction`, the secret/blocked sensitivity rules, the `max_bytes` cap)
runs before any backend sees the text, and the settings-aware
`SensitivityClassifier::redact` — **not** the bare `Redactor` — is the caller
surface for redaction (see [section 9](#9-sensitivity-and-redaction)).

**Output.** When the user clicks *Save as new entry* in
`ActionInspector.svelte`, the frontend invokes the separate `save_ai_result`
Tauri command which writes
the text via `runtime.add_text()` and returns the resulting `EntryDto`. The
persistence is intentionally a second user-driven step rather than a side effect
of the action.

---

## 15. Internationalization

Nagori ships globally but originated from a Japanese-speaking team, so
locale handling is part of the product contract.

**Surface map:**

| Surface | Localized? | Where strings live |
|---------|------------|--------------------|
| Tauri WebView UI | yes | `apps/desktop/src/app/lib/i18n/locales/<tag>.ts` |
| CLI help / output | no (English only) | `clap` attributes in `nagori-cli` |
| IPC error / audit | no (stable codes) | `IpcError.code`, `AuditEvent::event_kind` |
| Domain enums | no (stable variants) | `nagori-core` (`Sensitivity`, `RankReason`, …) |
| Tracing logs | no (English only) | `tracing` events |

The Rust core never holds user-facing translated copy. Classifiers and
rankers return enums or stable codes; the frontend (or any future
consumer) maps those to human-readable strings per locale.

**Frontend module.** `apps/desktop/src/app/lib/i18n/`

- `index.svelte.ts` — reactive locale store (`preference` + resolved
  `locale`), `messages()`, `setLocale`, `detectInitialLocale` /
  `detectSystemLocale`, locale negotiation.
- `locales/{en,ja,ko,zh-Hans,zh-Hant,de,fr,es}.ts` — English is the
  source of truth and defines the `Messages` interface; every other
  locale must satisfy it structurally.

Rules:

- No runtime fallback per key. A missing translation is a TypeScript
  compile error.
- Plural / count-aware strings are exposed as functions
  (`(count: number) => string`); per-locale files decide rendering.
  No ICU MessageFormat dependency.
- Date formatting goes through `Intl.DateTimeFormat` with a tag
  derived from the active locale (`en-US`, `ja-JP`, …).

**Persistence.** `AppSettings` carries a `Locale` enum
(`System` / `En` / `Ja` / `Ko` / `ZhHans` / `ZhHant` / `De` / `Fr` /
`Es`) serialized as a BCP-47-ish tag (`"system"` / `"en"` / `"ja"` /
`"ko"` / `"zh-Hans"` / `"zh-Hant"` / `"de"` / `"fr"` / `"es"`) and an
`Appearance` enum (`Light` / `Dark` / `System`). The casing of
`zh-Hans` / `zh-Hant` is preserved because the script subtag is the
canonical disambiguator for Simplified vs. Traditional Chinese.
`Locale::System` is the **default**: it is a persisted *preference*,
not a dictionary key — the frontend resolves it to a concrete locale
on every load by re-reading the OS / WebView language preferences, so
changing the OS language follows through without touching settings.
`Appearance::System` follows the OS theme; explicit light or dark sets
`<html data-theme>` directly. System mode paints from
`prefers-color-scheme` on first load, then pins the concrete OS theme
read via Tauri's `Window.theme()` and re-applies it on every
`onThemeChanged` event. The Tauri path is what makes a live OS light/dark
switch land on Windows, where WebView2 only samples
`prefers-color-scheme` when the webview is created — so the media query
alone went stale until the app was restarted.

**Negotiation.** Whenever the resolved locale is needed (`'system'`
preference or first paint with no settings yet), read
`navigator.languages`, strip region, lowercase, and pick the first
match in `SUPPORTED_LOCALES`. `zh-*` splits on the script subtag —
`zh-Hant` and the region-only Traditional tags (`zh-TW`, `zh-HK`,
`zh-MO`) route to Traditional; every other `zh-*` (including bare
`zh`) routes to Simplified. Unmatched preferences fall through to
`en`. `document.documentElement.lang` is updated whenever `setLocale`
is called so WebView accessibility / spellcheck behave correctly.

**Live propagation across webviews.** Settings runs in its own webview
with its own module stores, so a change made there reaches the palette
only through the `nagori://settings_changed` broadcast (the same
`AppSettings` watch-channel snapshot the tray / hotkey reconcile
consumes). On each broadcast `App.svelte` re-applies `appearance` and
`locale` straight from the payload — both live *outside* `settingsState`
(the theme is `<html data-theme>` / CSS state, the locale is the i18n
module's own `$state`) — and adopts the rest of the snapshot into
`settingsState` via `applySettingsSnapshot` (no extra `getSettings`
round-trip), so the `$derived` palette surfaces (row count, preview
pane, palette hotkeys, paste-format default) update live instead of
staying pinned to the launch-time values. `recentOrder` is applied
backend-side as a search runs, so the palette re-issues the current
query when it changes to re-sort the visible list. A generation counter
guards the adoption: a slow in-flight `refreshSettings` (kicked off at
palette mount or on window focus) discards its own stale settings
write-back rather than clobbering a fresher broadcast.

**Optimistic concurrency on settings writes.** The `settings` row carries
a monotonic `revision` token (migration `102`), bumped on every persisted
write. Single-field mutations (`set_capture_enabled`, onboarding markers)
go through `mutate_settings`, a read-modify-write under the runtime's
`settings_write_lock` that is inherently safe. The full-blob path the
settings window uses (`update_settings`) is the lost-update risk: it
edits a snapshot loaded earlier, so a concurrent tray pause/resume could
be silently reverted when the stale blob lands. `update_settings`
therefore carries the `revision` the window last observed as a
compare-and-swap base; `save_settings_checked` rejects the write with
`AppError::Conflict` (`settings_conflict`) when the stored revision has
moved. The settings window keeps that base fresh from the
`settings_changed` broadcast (which stamps the live revision), so a
conflict only occurs in the narrow window between dispatch and the
broadcast and is cleared transparently by the autosave retry. The
revision is tracked outside the autosave snapshot and pinned to `0` in
the dedup JSON so it never churns the idempotent-IPC guard.

**Adding a locale.**

1. Add `apps/desktop/src/app/lib/i18n/locales/<tag>.ts` typed
   `Messages`.
2. Add the tag to `SUPPORTED_LOCALES`, `MESSAGES`, and `DATE_TAGS` in
   `index.svelte.ts`; extend `negotiateOne` so OS-derived BCP-47
   variants route to it.
3. Add `Locale::<Tag>` in `nagori-core/src/settings.rs` and to
   `LocaleDto` (both `From` arms) in the Tauri DTO module.
4. Extend the `Messages.locales` type (in `en.ts`) and add the
   human-readable name under `locales.<tag>` in every existing
   dictionary so the picker can render it.
5. Add the new tag to `LocaleSetting` in
   `apps/desktop/src/app/lib/types.ts`.

CLI output, tracing events, and IPC / command error codes stay English
on purpose: agents and shell scripts are the primary consumers of
those surfaces, and English is the lowest-friction contract. Each
error code may carry a localized message on the frontend keyed off the
code.

---

## 16. Desktop shell integration

Wired in `apps/desktop/src-tauri/src/lib.rs`, all reacting to a single
`tokio::sync::watch` channel that broadcasts every `AppSettings`
change.

- **Tray (`tauri::tray::TrayIcon`)** — system tray icon (macOS menu
  bar, Windows notification area, Linux StatusNotifierItem /
  `libayatana-appindicator`) with *Show Palette*, *Pause Capture* /
  *Resume Capture* (label tracks `capture_enabled`), *Settings…*,
  *Clear History* (hard-deletes every non-pinned entry — pinned rows are
  kept — then emits `CLIPBOARD_CHANGED_EVENT` so an open palette refreshes
  and confirms via a notification, mirroring the `ClearHistory` secondary
  hotkey), *Quit Nagori*. The settings entry emits the Tauri event
  `nagori://navigate` with payload `"settings"`; the frontend listens
  via `@tauri-apps/api/event` and switches its route. Visibility is
  gated by `AppSettings.show_in_menu_bar`; toggling the setting hides
  or re-shows the tray icon at runtime. Install failures on Linux
  sessions without `StatusNotifierItem` support are logged and the rest
  of the app stays usable through the in-window controls.
- **macOS activation policy** — when `tray::install` succeeds,
  `setup()` calls `app.set_activation_policy(ActivationPolicy::Accessory)`
  on macOS so no Dock icon ever appears and the app is hidden from
  Cmd+Tab, matching the per-window `skipTaskbar: true` intent. The
  Dock entry is controlled per-process by NSApp's activation policy
  (not per-window), so without `Accessory` it would flicker in and
  out every time the palette is shown / hidden. The flip is gated on
  tray install actually succeeding: a session that has neither tray
  nor Dock icon would leave the (hidden) main window reachable only
  through the palette hotkey, which is a poor recovery path if the
  hotkey itself failed to register. The fallback branch returns
  early before this call, leaving the default `Regular` policy in
  place so the error window stays reachable via the Dock and
  Cmd+Tab even though tray installation is skipped there.
- **Auto-launch (`tauri-plugin-autostart`)** — wires the platform-native
  launcher: a `LaunchAgent` plist under `~/Library/LaunchAgents` on
  macOS, an `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
  registry entry on Windows, and a `~/.config/autostart/<bundle>.desktop`
  file on Linux. The settings subscriber keeps the launcher in sync
  with `AppSettings.auto_launch`; toggling the checkbox enables /
  disables the entry without a relaunch.
- **Secondary hotkeys** — `AppSettings.secondary_hotkeys`
  (`SecondaryHotkeyAction → accelerator`) is reconciled by the same
  watch channel. `RepasteLast` re-pastes the most recent entry;
  `ClearHistory` deletes every non-pinned row. Conflicts surface via
  the same `nagori://hotkey_register_failed` event used by the primary
  hotkey.
- **Clear-on-quit** — when `AppSettings.clear_on_quit` is true,
  `perform_exit_cleanup` (run from `RunEvent::ExitRequested` — i.e. tray
  Quit, `Cmd`/`Ctrl+Q`, dock-menu Quit) deletes non-pinned entries and
  purges the `nagori-preview/` plaintext temp cache before the tokio
  runtime tears down. Pinned entries are always preserved. The delete is
  bounded by a 1 s ceiling so a wedged DB cannot freeze the quit path, but
  the timeout no longer silently loses data: `perform_exit_cleanup` writes
  a `clear-on-quit.pending` marker (a sentinel file in the DB directory)
  *before* the delete and removes it only once the purge completes within
  the budget. On the next launch `complete_pending_clear_on_quit` runs
  synchronously during `setup` — before any window can show a row — and
  finishes the purge fail-closed if the marker is still present, so a
  timed-out / crashed / killed shutdown purge is always completed rather
  than leaving behind history the user asked to clear. The marker lives on
  the filesystem (not in the DB) so it survives even when the DB is the
  contended resource that timed the purge out.
  `WindowEvent::CloseRequested` is *not* a delete trigger: the same
  handler intercepts it on every OS, calls `prevent_close` and hides the
  main window so a `Cmd+W` / `Alt+F4` keystroke keeps the daemon (and the
  webview handle a later palette toggle relies on) alive.
- **Notifications (`tauri-plugin-notification`)** — one-shot "ready"
  alert after setup, plus a state-change toast when `capture_enabled`
  flips. Auto-paste failures emit `nagori://paste_failed`, which
  `emit_paste_failed` always routes to the palette (`"main"`) webview —
  toasts are palette-only, so the Settings window never subscribes. The
  payload carries a classified `reason`
  (`nagori_core::PasteFailureReason` → camelCase token:
  `accessibilityMissing` / `toolMissing` (+ `tool`) / `timeout` /
  `synthUnsupported` / `previousAppLost` / `unknown`) alongside the
  curated `error` string. Platform adapters raise
  `AppError::Paste { reason, message }` so the classification survives the
  `AppError → CommandError` collapse (which otherwise genericises
  `Platform` detail); the desktop command layer adds `PreviousAppLost`
  for a focus-restore failure that lives above the adapter. App.svelte
  records every failure in the `pasteDiagnostics` store so the StatusBar
  can leave a persistent diagnostic chip whose `title` is the localized
  per-reason remediation (e.g. "install `wtype`"); the chip is cleared on
  the next successful paste, on an Accessibility grant, or by manual
  dismiss, and an `accessibilityMissing` reason folds into the dedicated
  accessibility chip rather than stacking a second one. The
  palette suppresses the *toast* only for an `accessibilityMissing`
  failure in the not-yet-granted states the StatusBar accessibility chip
  already explains (`NotRequested` / `PromptShownNotGranted`); every other
  reason — and a genuine failure after a passive revoke
  (`RevokedAfterGranted`, whose detection is itself silent) — still
  renders, so the toast stays tied to a real paste attempt. A brief ✓
  toast confirms a fresh grant on the NotGranted→Granted transition,
  seeded from the first *hydrated* state (gated on `settingsState.loaded`)
  so an already-granted cold start does not flash a spurious
  confirmation. No-op silently if notification permission is not granted.
- **Startup fallback window** — when `AppState::try_new()` fails in
  `setup()` (Linux session whose compositor lacks `wl_data_control` /
  `ext_data_control`, denied data directory, corrupted SQLite file),
  the setup closure builds a small `WebviewWindow` labelled `fallback`
  whose contents are an inline `data:text/html;base64,...` document
  rendered by `fallback::build_fallback_html`. The page surfaces the
  annotated `AppError` (the same wording the CLI's
  `annotate_startup_error` / `annotate_linux_clipboard_error` emit on
  stderr) and links to `docs/platforms.md`. The error string is
  HTML-escaped before embedding so a crafted DB path cannot inject
  markup. `AppState` is intentionally left unmanaged in this branch,
  background tasks / tray / settings subscribers are skipped, and the
  fallback arm of `on_run_event` exits the process via
  `handle.exit(0)` when the user closes the window so the hidden main
  window cannot keep the app alive on macOS.
- **Permissions deep link** — the `request_accessibility` command
  drives `AXIsProcessTrustedWithOptions(prompt:YES)` on macOS, which
  surfaces the TCC dialog the first time and otherwise falls back to
  `open(1)` with the `x-apple.systempreferences:` URL so the Setup card
  can still hand the user the Accessibility pane. A failed `open(1)`
  (spawn error or non-zero exit) is propagated as a `CommandError`
  rather than swallowed, so the Setup card renders it as an inline error
  instead of silently dropping the user's only remaining route. The
  runtime also stamps `settings.onboarding.accessibilityPromptedAt`
  on `prompt = true` so the Setup card can later distinguish "never
  asked" from "asked and not granted".
- **Updater (`tauri-plugin-updater`)** — registered on every OS so
  `app.updater()` is always wired. `release.yaml` builds bundles for
  macOS (arm64 + x86_64), Windows x86_64 (NSIS), and Linux x86_64
  (`deb` + `AppImage`), and a dedicated `updater` job emits one
  consolidated signed `latest.json` covering every row in the matrix,
  so the availability probe runs on every supported OS. The MVP surface is read-only — the
  desktop shell calls `updater.check()` for the version comparison but
  does not call `update.download_and_install()`; users still follow
  the GitHub release link to upgrade. The wording differs by install
  medium so the user sees the right next step: bundles the updater
  *could* swap in place (`.app` / `.dmg`, NSIS, `AppImage`) show a
  "View release" link, while `deb` installs show "Download manually"
  to reflect that the GitHub artefact has to be re-installed by hand
  (no in-app `dpkg` prompt). The Rust side computes the medium gate
  in `commands::in_place_update_supported()` by delegating to
  `tauri::utils::platform::bundle_type()` — the same signal the
  plugin uses to pick a `latest.json` entry, so the UI advertisement
  and the underlying selection stay aligned — and exposes the result
  as `UpdateInfoDto.download_supported`. The plugin reads its
  endpoint and signing pubkey from `tauri.conf.json`
  (`plugins.updater`); the endpoint resolves
  `https://github.com/mhiro2/nagori/releases/latest/download/latest.json`
  via GitHub's "always points at the newest release asset" redirect,
  so no manifest needs to be edited per release. The bundle config sets
  `createUpdaterArtifacts: true` so the matching `.sig` sidecars land
  next to each bundle. The `bundle` matrix runs in parallel with
  `includeUpdaterJson: false` on `tauri-action@v0.6.2`, because that
  action's `latest.json` step does an unlocked read-modify-write on a
  single shared release asset and parallel rows would race it; instead
  the downstream `updater` job assembles the manifest once from the
  uploaded signatures, and `publish` depends on `updater`. The `commands::check_for_updates` Tauri command
  wraps `updater.check()` and is surfaced as the "Check for updates
  now" button under Settings → Advanced.
  `AppSettings.auto_update_check`, when enabled, drives the one-shot
  startup probe in `spawn_startup_update_probe`, which surfaces
  availability via an OS notification (the same path used by
  capture/AI state changes). It is also the single network-affecting
  toggle the daemon honours: `nagori doctor` reports it under
  `auto_update_check` so operators can see at a glance whether any
  background network call is permitted. The manual "Check for updates
  now" button bypasses the toggle by design — it is an explicit user
  action.
  `AppSettings.update_channel` (currently fixed to `Stable`) is
  persisted so future Beta/Nightly channels can land without a
  settings migration. The signing keypair is generated once per
  release line via
  `pnpm --dir apps/desktop exec tauri signer generate`; the private
  half lives in the GitHub Actions secrets `TAURI_SIGNING_PRIVATE_KEY`
  / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, the public half is committed
  into `tauri.conf.json`. `release.yaml` fails fast when that pubkey
  is empty so an unverifiable bundle never ships. The macOS `.app` /
  `.dmg` are not codesigned (Gatekeeper warns on first launch) and the
  Windows NSIS bundle is not Authenticode-signed (SmartScreen warns on
  first launch), even though the Tauri-side `minisign` signature
  verifies on every platform.

---

## 17. Observability

Tracing is the single source of truth. Events:

```
capture_started · capture_skipped · entry_inserted · entry_blocked
search_started · search_completed
paste_started · paste_completed
ai_action_started · ai_action_completed
permission_missing
```

**Never log full clipboard content by default.** Previews are
truncated; `Sensitivity::Secret | Blocked` payloads are referenced by
`entry_id` only. Tracing events are English-only; operator-facing logs
are intentionally not localized to keep grep recipes portable.

`audit_events` rows carry the same machine-readable `event_kind` as the
tracing event so the desktop UI and operators see the same vocabulary.

---

## 18. Testing

**Unit.** Domain conversions, deduplication, sensitivity classification,
search normalization, n-gram generation, ranking, retention policy,
redaction parity.

**Integration.** SQLite repositories, FTS5 + ngram search, capture
→ index pipeline, CLI against a test database, IPC request / response
round-trips, search-cache invalidation under concurrent
mutation/search, settings watcher fan-out.

**Platform.** Manual or feature-gated tests for macOS clipboard
read/write, global hotkey registration, auto-paste, and permission
detection. The `apps/desktop` end-to-end tests drive a real Tauri
shell and a synthetic clipboard producer.

**Benchmarks.** Search at 1k / 10k / 100k entries, Japanese queries,
long-text entries, repeated prefix typing. Target: top-50 results
under 80 ms for 100k text entries on a developer machine.

---

## 19. Security notes

- **Capture** — no remote network calls; the capture loop never
  leaves the process.
- **Storage at rest** — SQLite file forced to `0600`, parent
  directory to `0700`. The DB itself is **not** encrypted, and
  shipping 1.0 without encryption is a documented, accepted decision
  (see `docs/security-encryption-at-rest.md` → Release decision):
  permission bits keep other local users out but do not defend
  against backups, sync clients, or code running as the same user.
  README documents the gap and recommended mitigations (avoid sync
  targets, rely on FileVault, prefer `Store redacted`). To keep
  *deleted* secrets from lingering in the live file, every connection
  runs `secure_delete = ON` (freed pages zeroed) and the explicit
  purge paths (`clear_non_pinned`, `clear_older_than`) issue
  `wal_checkpoint(TRUNCATE)` so the pre-deletion content does not
  survive in WAL frames — residue reduction, not a substitute for
  full-disk encryption (freed disk blocks stay recoverable at the
  filesystem layer until reused). The SQLCipher / OS-keystore
  trade-offs are captured in `docs/security-encryption-at-rest.md`.
- **Image streaming** — the `nagori-image://` Tauri scheme handler
  returns 403 for `Sensitivity::Private | Secret | Blocked` so secret
  imagery never reaches the WebView. The `/thumb/<id>` branch
  re-asserts the same gate before serving a cached row, and the
  generator refuses to write a thumbnail for non-Public sensitivities,
  so the derived raster cache cannot become a side-channel for
  classified content.
- **Image payload validation** — external clipboard producers freely
  label arbitrary bytes as `image/*`, so the workspace treats every
  raster payload as untrusted. `nagori_core::image_signature::detect`
  is the single source of truth for the supported set (PNG, JPEG, GIF,
  WebP, BMP, TIFF); SVG is deliberately excluded because it can host
  script. The factory consults the detector at *capture time* and
  drops representations whose magic number disagrees with the declared
  MIME, falling through to sibling text/HTML reps when present; the
  Tauri scheme handler runs the same check at *serve time* before the
  bytes reach the WebView, returning 415 on mismatch. Combined with
  `X-Content-Type-Options: nosniff` this gives three independent gates
  against a payload like an HTML body mislabelled as `image/png`.
  Rejections are logged with declared MIME, detected MIME, and byte
  count; raw bytes are never written to the log.
- **Decoded-image pixel cap** — `max_entry_size_bytes` only inspects
  encoded bytes, but `image::decode().to_rgba8()` materialises the
  full `width × height × 4` buffer. A few-KB PNG advertising
  65535×65535 would allocate ~16 GB before any byte budget fires.
  `nagori_core::MAX_DECODED_IMAGE_PIXELS` (64 MP → ~256 MB worst-case
  RGBA, above an 8K screenshot) is the platform-wide guard. The
  Windows capture path probes whichever format `arboard::get_image`
  will read first — registered `"PNG"` wins if available, otherwise
  `CF_DIBV5` / `CF_DIB` — and bails out before allocating. The PNG
  probe reads the IHDR chunk directly from the 24-byte signature +
  length + type + width + height prefix, deliberately avoiding
  `ImageReader::into_dimensions` because the latter advances to IDAT
  and a real PNG with ancillary chunks (gAMA / sRGB / pHYs) would
  silently fail a short-prefix probe. DIB dimensions come from the
  shared 12-byte `BITMAPINFOHEADER` prefix. Oversized payloads get an
  `image_rep_dropped reason=decoded_pixels_exceed_cap` warn log;
  sibling text / file-list reps still capture. Copy-back
  (`write_image_bytes`, `build_dibv5_payload`) probes the same way
  and surfaces `AppError::Unsupported` so the daemon refuses to decode
  an attacker-controlled canvas. The cap is intentionally not
  user-tunable.
- **AI** — remote providers are off by default. The classifier runs
  before any provider call, and `AiInputPolicy::require_redaction`
  forces the canonical scrubber on the payload.
- **IPC** — Unix-domain socket (macOS / Linux, `0600` mode) or Win32
  named pipe (Windows, default named-pipe security descriptor, no
  custom DACL — `reject_remote_clients(true)` blocks UNC peers but a
  local user can still open the pipe). Authentication therefore relies
  on a per-launch token file (`0600` on Unix; default NTFS permissions
  inherited from `%LOCALAPPDATA%\nagori\` on Windows). Tight read
  timeouts on the unauthenticated handshake (`FIRST_READ_TIMEOUT` 1 s,
  `READ_TIMEOUT` 3 s) cap slow-loris pressure on the 32 connection
  permits; no TCP listener. Token verification uses constant-time
  comparison.
- **Tauri command ACL** — `build.rs` declares every `generate_handler!`
  command in `tauri_build::AppManifest::commands`, which flips app
  commands from "callable by any window by default" to deny-by-default:
  a webview invocation is rejected unless a capability for *that* window
  grants `allow-<command>`. The grants are split per window —
  `capabilities/palette.json` (the `main` palette webview: search,
  paste/copy/delete/pin, preview, the AI action inspector, the
  status-bar capture toggle, plus the shared read-only stores) and
  `capabilities/settings.json` (the `settings` webview: `update_settings`,
  the password-manager preset, the updater check, `install_cli`,
  `request_accessibility`, the AI/semantic controls, plus the same shared
  stores). A compromise of the palette webview therefore cannot reach
  `update_settings` / `install_cli` / `request_accessibility`, and a
  compromise of the settings webview cannot drive clipboard side effects.
  Commands no webview invokes (`clear_history`, `add_entry`,
  `repaste_last`, the unused list/get/copy wrappers, `toggle_palette`,
  `close_settings`) are in the manifest but granted to neither window, so
  they stay unreachable from a webview entirely; the autogenerated
  permission TOMLs live under `permissions/autogenerated/`. The command
  list in `build.rs` must stay in lockstep with `generate_handler!`: a
  command registered but absent from the manifest has no permission and
  is rejected for every window.
- **CLI** — `--include-sensitive` is required to print secret bodies;
  default `--json` output redacts them. Mutating commands have stable
  exit codes so agents fail loudly.

---

## 20. Product evolution

```text
clipboard history → per-app filters → embedding / semantic recall
   → editor & browser integrations → multi-device sync (opt-in)
```

The crate boundaries assume daemon separation, action-runner
plurality, and a stable IPC schema so this path can be walked
without rewriting the core.

Concrete near-term extensions: embedding index, local vector
database, per-project clipboard scopes, app-specific history filters,
editor integrations, browser extension, clipboard workflows, plugin
API, opt-in cloud sync, team vaults, mobile companion app.

---

## 21. Checklist for new work

- Does it keep **core logic target-agnostic** (no Tauri / SQLite / OS
  APIs in `nagori-core`)?
- Are surfaces **thin** (CLI / Tauri command / IPC handler all just
  call into `NagoriRuntime`)?
- Is it **deterministically testable** (fixtures, snapshot output,
  no real clipboard or network)?
- Are **public types** Nagori-owned, not leaked third-party internals
  (`rusqlite::Row`, `NSPasteboard`, etc.)?
- Does **business logic** land in `nagori-core` / `nagori-search` /
  `nagori-storage` rather than the desktop shell or CLI?
- Does it respect the **privacy contract** (no remote calls without
  opt-in, classifier runs first, secret bodies redacted by default)?
- Does it **invalidate the search cache** on every corpus mutation,
  before and after the storage write?

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
14. [AI actions](#14-ai-actions)
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
   `NagoriRuntime` so capture, search, paste, and AI actions behave
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
│                  │ │                  │ │    linux}        │
│                  │ │                  │ │ nagori-ai        │
│                  │ │                  │ │ nagori-ipc       │
└──────────────────┘ └──────────────────┘ └──────────────────┘
```

| Crate | Role |
|-------|------|
| `nagori-core` | Domain model, sensitivity policy, repository traits, `SearchService` orchestration, settings, errors |
| `nagori-storage` | SQLite (rusqlite) repositories, FTS5 / ngram tables, migrations, image blob handling |
| `nagori-search` | Text normalization, CJK n-gram tokenizer, default ranker, semantic search hooks |
| `nagori-platform` | Cross-platform traits: clipboard read/write, paste, hotkey, permissions, frontmost window |
| `nagori-platform-macos` | NSPasteboard capture, Cmd+V auto-paste, Accessibility checks, frontmost-app metadata |
| `nagori-platform-windows` | Windows adapter — stub today (every method returns `Unsupported`); shape preserved for future port |
| `nagori-platform-linux` | Linux adapter — stub today (every method returns `Unsupported`); shape preserved for future port |
| `nagori-ai` | AI provider trait, local mocks, OpenAI provider, action registry, redactor |
| `nagori-ipc` | Newline-delimited JSON over Unix domain sockets, auth-token handshake, request/response DTOs |
| `nagori-daemon` | `NagoriRuntime` façade, capture loop, maintenance jobs, IPC server, in-memory search cache |
| `nagori-cli` | `nagori` binary; clap commands, plain/JSON/JSONL output, IPC client + read-only DB fallback |
| `apps/desktop` | Tauri 2 shell + Svelte 5 frontend; thin command layer over `NagoriRuntime` |

Repository layout (abbreviated):

```text
apps/
  desktop/                  # Tauri + Svelte palette and settings UI
crates/
  nagori-core/ nagori-storage/ nagori-search/
  nagori-platform/ nagori-platform-macos/
  nagori-platform-windows/ nagori-platform-linux/
  nagori-ai/ nagori-ipc/ nagori-daemon/ nagori-cli/
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
  └─ AppState              · capture loop      + AI provider
       └ spawns tasks      · settings sub      + platform adapters
                           · maintenance

nagori-cli (`--ipc` / `--auto-ipc`)
  └─ IpcClient ──► Unix socket ──► IpcRequest / IpcEnvelope
                                       │
nagori-cli `daemon run`                ▼
  └─ run_daemon ──► accept_loop ──► NagoriRuntime.handle_ipc
```

Two execution modes:

- **In-process (desktop)** — the Tauri shell builds a `NagoriRuntime`
  via `NagoriRuntimeBuilder` and Tauri commands call its methods
  directly. `AppState::spawn_background_tasks` and
  `spawn_settings_subscribers` (`apps/desktop/src-tauri/src/lib.rs`)
  start the capture loop and settings fan-out.
- **Out-of-process (daemon + CLI)** — `nagori daemon run` calls
  `nagori-daemon::serve::run_daemon`, which spawns the same kind of
  background tasks plus the Unix-socket accept loop, then dispatches
  every request through `NagoriRuntime::handle_ipc`. CLI calls with
  `--ipc <socket>` / `--auto-ipc` route through that socket; `--db
  <path>` is a read/write fallback that bypasses the daemon and is
  documented as **repair / offline mode** in
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
ClipboardReader.current_sequence()         (cheap pre-check)
  → frontmost_app() snapshot               (before reading the body)
  → ClipboardReader.current_snapshot()     (full pasteboard read)
  → EntryFactory.from_snapshot()           (decode → ClipboardEntry +
                                            SHA-256 content hash +
                                            search document)
  → kind guard  (settings.capture_kinds)
  → size guard  (settings.max_entry_size_bytes)
  → SensitivityClassifier.classify()       (built-in detectors +
                                            app_denylist + user regexes)
        ├─ Blocked → audit + drop
        └─ otherwise → take redacted preview
  → SecretHandling
        ├─ Block         → audit + drop
        ├─ StoreRedacted → rewrite body / hash / FTS / ngrams
        └─ StoreFull     → keep raw bytes
  → search-cache invalidate (pre)
  → EntryRepository.insert()               (single SQLite tx writes
                                            entries + search_documents
                                            + search_fts + ngrams)
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
  classification both live in the capture loop.
- `app_denylist` is enforced inside `SensitivityClassifier::classify`
  against the snapshot's `source` (bundle id / name), not in the
  factory.
- `EntryRepository::insert` upserts `entries`, `search_documents`, the
  `search_fts` virtual table, and `ngrams` in one SQLite transaction,
  so search is consistent the moment the row commits — there is no
  separate `SearchRepository.upsert_document` step in the live path.
- The cache is invalidated on **both** sides of the insert; see
  [section 8](#8-search) for why.

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
  derived metadata (counts, normalized URL, language hint).
- `ImageContent` carries a `PayloadRef` plus optional in-memory
  `pending_bytes` that flow from capture → factory → storage; after
  insertion the bytes live in `entries.payload_blob` and the field is
  always `None` post-deserialisation.
- `RichTextContent` keeps `plain_text` (for FTS / ngrams) and an optional
  `markup` payload tagged `Html` or `Rtf` for preview rendering.
- `FileListContent` flattens `NSPasteboardTypeFileURL` URLs into POSIX
  paths plus a `display_text` newline-joined form for search.

**`PayloadRef`** — `InlineText` / `DatabaseBlob(String)` /
`ContentAddressedFile { sha256, path }`. Today images use
`DatabaseBlob`; the variant exists so a future content-addressed store
can be plugged in without changing the entry model.

**`EntryMetadata`** — timestamps, source app (`bundle_id`, `name`,
`executable_path`), use count, `ContentHash` (SHA-256 — used for dedup).

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
| `entries` | Full entry rows. Image bytes live inline in `payload_blob` / `payload_mime`. |
| `search_documents` | Title, preview, normalized text per entry — the source of truth for what FTS / ngrams index. |
| `search_fts` | FTS5 virtual table over `title` / `preview` / `normalized_text` (`unicode61`). |
| `ngrams` | `(gram, entry_id, position)` triples for CJK partial-match lookup, capped at `MAX_NGRAM_INPUT_CHARS` (4096) characters per entry. |
| `settings` | Key/value persistence for `AppSettings`. |
| `audit_events` | Capture / policy events (block, redact, etc.). Never stores raw clipboard content. |

**Image bytes** stay inline because typical clipboard images are sub-MiB
and SQLite handles that size cheaply; flowing them through a
content-addressed file store was not worth the extra failure modes for
the size class. The frontend streams them lazily via the
`nagori-image://` Tauri custom URI scheme so the WebView fetches
`nagori-image://localhost/<entry_id>` like any other `<img src>`. The
handler returns 403 for `Sensitivity::Private | Secret | Blocked` so
secret imagery never reaches the WebView.

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
exact substring + recency.

---

## 9. Sensitivity and redaction

**Detectors** (`nagori-core::policy`): API-key-like strings, JWTs, SSH
private keys (PEM blocks tolerate a missing END marker), AWS access
keys, GitHub tokens, Luhn-checked credit-card runs, OTP-like 6–8 digit
short codes, password-manager source apps, and user-defined regex.

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
| `ClipboardReader` | `current_snapshot()`, `current_sequence()` for the capture loop |
| `ClipboardWriter` | Restore an entry to the OS clipboard |
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
- **Windows** (`nagori-platform-windows`) and **Linux**
  (`nagori-platform-linux`) — present as crates but every trait method
  currently returns the `Unsupported` error. Nagori is macOS-only at
  this time; these crates exist so cross-platform porting work can land
  incrementally without disturbing the trait surface.

**Permission model.** The platform layer exposes:

```rust
enum PermissionKind { Accessibility, InputMonitoring, Clipboard,
                       Notifications, AutoLaunch }

enum PermissionState { Granted, Denied, NotDetermined, Unsupported }
```

`PermissionChecker::check()` returns the live state for every kind.
The capture loop and copy paths only need `Clipboard`. The auto-paste
path needs `Accessibility`; when it is missing, the desktop and CLI
both fall back to **copy-only** behaviour (palette `Enter` and
`nagori paste` write to the clipboard but skip the Cmd+V synthesis).
The onboarding banner and `nagori doctor` surface the missing
permission so the user can fix it.

---

## 11. IPC boundary

**Transport.** Newline-delimited JSON over Unix domain sockets. The
client writes one `IpcEnvelope { token, request: IpcRequest }` line
and reads one `IpcResponse` line. Default socket lives at
`~/Library/Application Support/nagori/nagori.sock`; an auth-token file
sits next to it as `nagori.token` with `0600` permissions, and the
server rejects envelopes whose token does not match.

**Request / response types** (`nagori-ipc::protocol`):

```rust
enum IpcRequest {
    Search, GetEntry, ListRecent, ListPinned,
    AddEntry, CopyEntry, PasteEntry,
    DeleteEntry, PinEntry, Clear,
    RunAiAction,
    GetSettings, UpdateSettings,
    Doctor, Health, Shutdown,
}

enum IpcResponse {
    Search, Entry, Entries,
    AiOutput, Cleared,
    Doctor, Health,
    Ack, Error,
}
```

Permissions are not exposed over IPC; CLI clients query them through
`nagori doctor`, which is a separate request that aggregates version,
paths, daemon status, and `PermissionChecker::check()` output.

**Backpressure & limits.** `MAX_IPC_BYTES` caps per-message size. The
server uses an `accept_loop` over a `bind_unix` listener; each
connection runs on a tokio task and shares the same `NagoriRuntime`.

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
- `open_accessibility_settings` — shells out to `open(1)` with the
  `x-apple.systempreferences:` URL for the onboarding deep link.

Both are deliberately scoped to UI focus / shell integration and do
not duplicate runtime logic.

**Frontend layout** (`apps/desktop/src/app/components/`):

- `Palette.svelte` — top-level container. Stacks `SearchBox` →
  `OnboardingBanner` → (`ResultList` + `PreviewPane`) → `StatusBar`.
- `OnboardingBanner.svelte` — only renders when `get_permissions`
  reports Accessibility as missing; offers an *Open System Settings*
  deep-link.
- `StatusBar.svelte` — entry count, last-search elapsed time, capture
  badge, AI badge, keyboard hints.
- `ResultItem.svelte` — kind-aware row renderer. URL rows emphasise
  the domain; code rows show a heuristic language badge (TS, RS, PY,
  JSON, …) inline.
- `PreviewPane.svelte` — hydrates full preview lazily through
  `get_entry_preview`; includes a token-based syntax highlighter for
  `code` / `url` kinds.
- `ActionMenu.svelte` — modal for AI actions. The result block shows
  *Copy* (uses `navigator.clipboard`) and *Save as new entry* (calls
  `save_ai_result`).
- `SettingsView.svelte` — tabbed *General* / *Privacy* / *AI* / *CLI*
  / *Advanced* settings panel. Denylists are edited as multi-line
  textareas serialised back into `string[]`; capture kinds, paste
  format, recent ordering, total storage limit, and appearance are
  exposed as structured controls.

The `stores/settings.svelte.ts` store is the single source for
`captureEnabled()`, `aiEnabled()`, `accessibilityState()`, and
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

**Doctor.** `nagori doctor` prints version, paths, daemon status, and
permission states (Accessibility, Input Monitoring, Notifications,
AutoLaunch) — the canonical first step for support tickets.

---

## 14. AI actions

`nagori-ai` exposes:

- `AiProvider` trait (local / OpenAI / mock implementations).
- Action registry (`Summarize`, `Translate`, `FormatJson`,
  `FormatMarkdown`, `ExplainCode`, `Rewrite`, `ExtractTasks`,
  `RedactSecrets`).
- Prompt templates per action.
- A `Redactor` for shaping payloads before they leave the machine.

**Privacy contract.** `local_only_mode` is the default: remote
providers are off until the user opts in. Even with remote enabled,
`AiInputPolicy` decides whether redaction is required (`require_redaction
= true` for any action invoked on a `Secret` entry) and whether the
payload exceeds `max_bytes`. When the classifier returns `Blocked` the
action returns `PolicyError` without contacting the provider.

**Output.** `run_ai_action` returns `AiOutput` (text + provider
warnings). When the user clicks *Save as new entry* in
`ActionMenu.svelte`, the frontend invokes the separate `save_ai_result`
Tauri command which writes the text via `runtime.add_text()` and
returns the resulting `EntryDto`. The persistence is intentionally a
second user-driven step rather than a side effect of the AI call.

The caller surface for settings-aware redaction is
`SensitivityClassifier::redact`, **not** the bare `Redactor` — see
[section 9](#9-sensitivity-and-redaction).

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

- `index.svelte.ts` — reactive locale store, `messages()`, `setLocale`,
  `detectInitialLocale`, locale negotiation.
- `locales/{en,ja,ko,zh-Hans}.ts` — English is the source of truth and
  defines the `Messages` interface; every other locale must satisfy it
  structurally.

Rules:

- No runtime fallback per key. A missing translation is a TypeScript
  compile error.
- Plural / count-aware strings are exposed as functions
  (`(count: number) => string`); per-locale files decide rendering.
  No ICU MessageFormat dependency.
- Date formatting goes through `Intl.DateTimeFormat` with a tag
  derived from the active locale (`en-US`, `ja-JP`, …).

**Persistence.** `AppSettings` carries a `Locale` enum
(`En` / `Ja` / `Ko` / `ZhHans`) serialized as a BCP-47-ish tag
(`"en"` / `"ja"` / `"ko"` / `"zh-Hans"`) and an `Appearance` enum
(`Light` / `Dark` / `System`). The casing of `zh-Hans` is preserved
because it is the canonical script subtag and the frontend negotiation
maps any `zh-*` regional preference onto it. `Appearance::System`
is the only mode that consults `prefers-color-scheme`; explicit light
or dark sets `<html data-theme>` directly.

**Negotiation.** First launch with no saved preference: read
`navigator.languages`, strip region, lowercase, first match in
`SUPPORTED_LOCALES` wins, otherwise default to `en`.
`document.documentElement.lang` is updated whenever `setLocale` is
called so WebView accessibility / spellcheck behave correctly.

**Adding a locale.**

1. Add `apps/desktop/src/app/lib/i18n/locales/<tag>.ts` typed
   `Messages`.
2. Add the tag to `SUPPORTED_LOCALES` and `MESSAGES` in
   `index.svelte.ts`.
3. Add `Locale::<Tag>` in `nagori-core/src/settings.rs` and to
   `LocaleDto` in the Tauri DTO module.
4. Add the human-readable name under `locales.<tag>` in every existing
   dictionary so the picker can render it.

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

- **Tray (`tauri::tray::TrayIcon`)** — menu-bar icon with *Show
  Palette*, *Pause Capture* / *Resume Capture* (label tracks
  `capture_enabled`), *Settings…*, *Quit Nagori*. The settings entry
  emits the Tauri event `nagori://navigate` with payload `"settings"`;
  the frontend listens via `@tauri-apps/api/event` and switches its
  route. Visibility is gated by `AppSettings.show_in_menu_bar`; toggling
  the setting installs or removes the tray icon at runtime.
- **Auto-launch (`tauri-plugin-autostart`)** — registers a
  `MacosLauncher::LaunchAgent` on demand. The settings subscriber keeps
  the LaunchAgent in sync with `AppSettings.auto_launch`; toggling the
  checkbox enables / disables the agent without a relaunch.
- **Secondary hotkeys** — `AppSettings.secondary_hotkeys`
  (`SecondaryHotkeyAction → accelerator`) is reconciled by the same
  watch channel. `RepasteLast` re-pastes the most recent entry;
  `ClearHistory` deletes every non-pinned row. Conflicts surface via
  the same `nagori://hotkey_register_failed` event used by the primary
  hotkey.
- **Clear-on-quit** — when `AppSettings.clear_on_quit` is true, the
  main window's `WindowEvent::CloseRequested` handler synchronously
  deletes non-pinned entries before tear-down. Pinned entries are
  always preserved.
- **Notifications (`tauri-plugin-notification`)** — one-shot "ready"
  alert after setup, plus state-change toasts when `capture_enabled` or
  `ai_enabled` flip. Auto-paste failures (e.g. revoked Accessibility)
  emit `nagori://paste_failed`; the palette renders an in-window toast
  with a one-click jump into Settings. No-op silently if notification
  permission is not granted.
- **Permissions deep link** — the `open_accessibility_settings`
  command shells out to `open(1)` with the
  `x-apple.systempreferences:` URL so the onboarding banner can take
  the user directly to the Accessibility pane.

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
  directory to `0700`. The DB itself is **not** encrypted;
  permission bits keep other local users out but do not defend
  against backups, sync clients, or code running as the same user.
  README documents the gap and recommended mitigations (avoid sync
  targets, rely on FileVault, prefer `Store redacted`).
- **Image streaming** — the `nagori-image://` Tauri scheme handler
  returns 403 for `Sensitivity::Private | Secret | Blocked` so secret
  imagery never reaches the WebView.
- **AI** — remote providers are off by default. The classifier runs
  before any provider call, and `AiInputPolicy::require_redaction`
  forces the canonical scrubber on the payload.
- **IPC** — Unix-domain socket plus auth token file (`0600`); no TCP
  listener.
- **CLI** — `--include-sensitive` is required to print secret bodies;
  default `--json` output redacts them. Mutating commands have stable
  exit codes so agents fail loudly.

---

## 20. Product evolution

```text
clipboard history → per-app filters → embedding / semantic recall
   → editor & browser integrations → multi-device sync (opt-in)
```

The crate boundaries assume daemon separation, AI provider plurality,
and a stable IPC schema so this path can be walked without rewriting
the core.

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

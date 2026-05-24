# `nagori` CLI

The CLI is a single binary, `nagori`, that can either talk directly to the
SQLite database or to a running daemon over its IPC endpoint (Unix-domain
socket on macOS / Linux, Win32 named pipe on Windows).

## Modes

| Mode        | When to use                                                  | Flag                |
|-------------|--------------------------------------------------------------|---------------------|
| Direct DB   | Default. Opens the local SQLite history file                 |  *(none)*           |
| Auto IPC    | Try the daemon endpoint; fall back to direct DB if unreachable | `--auto-ipc`      |
| Forced IPC  | Always go through the daemon; error if the endpoint is missing | `--ipc <endpoint>` |

Defaults:

* **macOS** — DB at `~/Library/Application Support/nagori/nagori.sqlite`,
  socket at `~/Library/Application Support/nagori/nagori.sock`.
* **Windows** — DB under `%LOCALAPPDATA%\nagori\nagori.sqlite`, named pipe
  at `\\.\pipe\nagori`. Pass `--ipc \\.\pipe\<name>` to override.

Set `NAGORI_DB_PATH=/path/to/nagori.sqlite` to redirect the store away
from the platform default. The desktop shell honours the same variable,
so both processes target the same DB when launched with it.

## Output formats

Every read command supports both human and machine output:

* `--json` — pretty-printed single JSON document.
* `--jsonl` — newline-delimited JSON, one record per line.
* *(none)* — concise human table.

## Subcommands

### `nagori list [--limit N] [--pinned]`

Show recent or pinned entries.

```sh
nagori list --limit 5
nagori list --pinned --json
```

### `nagori search <query> [--limit N]`

Run the ranker. `query` is normalised to NFKC + lowercase before matching.

```sh
nagori search "kubectl"
nagori search "クリップ" --jsonl
```

### `nagori get <id> [--include-sensitive]`

Print one entry. By default the full text is suppressed for `Private` and
`Secret` entries; `--include-sensitive` opts back in.

### `nagori add [--text <s> | --stdin]`

Insert an entry. `--stdin` reads the full standard input as the payload.

### `nagori delete <id>` / `nagori pin <id>` / `nagori unpin <id>`

Mutate metadata. `delete` is soft (sets `deleted_at`).

### `nagori copy <id>` / `nagori paste <id>`

`copy` writes to the system clipboard. `paste` additionally synthesises
the platform paste shortcut into the frontmost app — Cmd+V on macOS
(via `CGEventPost`) or Ctrl+V on Windows (via `SendInput`).

### `nagori clear [--older-than-days N]`

Soft-delete unpinned entries. With no flag, deletes everything that is not
pinned. With `--older-than-days`, deletes entries created before that cutoff.

### `nagori ai <action> <id>`

Run a local AI action against an entry. Available actions: `summarize`,
`translate`, `format-json`, `format-markdown`, `explain-code`, `rewrite`,
`extract-tasks`, `redact-secrets`. `Secret` entries are blocked unless the
action is `redact-secrets`; `Private` entries are redacted before being sent.

### `nagori doctor`

Print database / IPC paths, capture and AI flags, and per-platform
permission status (TCC kinds on macOS; Clipboard / Accessibility =
`Granted` and the rest = `Unsupported` on Windows).

When `nagori doctor` successfully connects to a daemon via
`--ipc <endpoint>` (or runs alongside the desktop app's in-process
runtime), three extra health rows are included in the daemon-driven
report:

- `maintenance\t<ok|degraded>\tconsecutive_failures=N[\t<last error>]`
  — retention loop status (cleared on the next successful run).
- `capture\t<ok|degraded>\tconsecutive_failures=N\tlast_event=<category>[\t<last error>]`
  — steady-state capture-loop status. `consecutive_failures` counts
  adapter / settings-load / storage errors (intentional drops do not
  contribute); `last_event` is the most recent non-success category,
  one of `none`, `adapter`, `settings_load`, `storage`, `policy`,
  `oversized_drop`. The desktop tray surfaces the same degraded state
  in its tooltip so the CLI and tray never disagree.
- `startup\t<ready|failed|pending>[\t<last error>]` — outcome of the
  capture loop's pre-poll initialisation. `pending` means the host
  process is still loading settings; `failed` means the loop aborted
  before polling (typically a settings-load error) and is the same
  signal the desktop app reads when deciding whether to surface a
  "Nagori is running" notification.
- `ipc\tpanic_count=N[\t<last panic>]` — total IPC handler tasks the
  accept loop has observed to panic over this daemon's lifetime, plus
  the last panic message for one-glance triage. A non-zero count is a
  hint to grep the daemon log for the matching stack trace; it does not
  flip the top-level `ok` flag because a single panic does not have the
  same operational impact as a wedged retention loop.

`nagori doctor` also prints a thumbnail cache row in both
daemon-driven and direct-DB reports:

- `thumbnails\tused=<bytes>\tcap=<bytes|disabled>` — aggregate
  `entry_thumbnails` footprint versus the
  `max_thumbnail_total_bytes` LRU budget (default 64 MiB; `disabled`
  means the eviction sweep is off and the cache may grow unbounded).
  Thumbnails are derived from the original image payloads and are
  regenerable, so the row is a performance signal — a `used` value
  hovering near `cap` combined with sluggish image previews suggests
  raising the budget. If the footprint read fails, `used` falls back
  to `(unknown)`.

The local fallback (no daemon available — runs directly against the
SQLite store) prints settings / paths / permissions and that
`thumbnails` row; the maintenance / capture / startup health rows
require a daemon process to report on.

### `nagori capabilities`

Print the host adapter's static capability matrix — what nagori can
do on this OS given the right permissions and tools — for each of
clipboard capture / copy-back / auto-paste / global hotkey /
frontmost-app / permission UI / updater. Rows use one of `available`,
`experimental`, `unsupported`, `requires_permission`, or
`requires_external_tool`; pair with `nagori doctor` to see the live
permission and tool state.

Works against a running daemon when `--ipc <endpoint>` is set (the
daemon returns the same matrix as a local probe, since the report is
static and wired in at startup), and falls back to a local probe
otherwise.

### `nagori daemon run [--capture-interval-ms N] [--maintenance-interval-min N]`

Boot the daemon. Holds the SQLite handle, runs the capture loop, and serves
the IPC endpoint (Unix socket on macOS / Linux, named pipe on Windows).
Available on macOS and Windows; other platforms exit with
"daemon run is only available on macOS and Windows in this build".

### `nagori daemon stop`

Send a shutdown request via IPC. Requires `--ipc <endpoint>`.

### `nagori daemon status`

Print IPC endpoint and DB metadata without contacting a daemon.

## Exit codes

The CLI returns the following exit codes. Codes `2`, `4`, `5`, `6`, `7`,
and `8` map onto the typed `AppError` variants returned from the
`nagori-core` services; `1` is the fallback when an underlying error
cannot be classified.

| Code | Meaning            | Underlying `AppError`                                |
|------|--------------------|------------------------------------------------------|
| 0    | Success            | —                                                    |
| 1    | Generic error      | (untyped `anyhow::Error`)                            |
| 2    | Invalid input      | `InvalidInput`                                       |
| 4    | Not found          | `NotFound`                                           |
| 5    | Policy violation   | `Policy`                                             |
| 6    | Permission denied  | `Permission`                                         |
| 7    | Unsupported        | `Unsupported` (e.g. unsupported `SearchMode`)        |
| 8    | Internal error     | `Storage`, `Search`, `Platform`, `Ai`                |

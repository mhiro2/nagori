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

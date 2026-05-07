# `nagori` CLI

The CLI is a single binary, `nagori`, that can either talk directly to the
SQLite database or to a running daemon over a Unix socket.

## Modes

| Mode        | When to use                                                  | Flag             |
|-------------|--------------------------------------------------------------|------------------|
| Direct DB   | Default. Opens `~/Library/Application Support/nagori/nagori.sqlite` |  *(none)*        |
| Auto IPC    | Try the daemon socket; fall back to direct DB if unreachable | `--auto-ipc`     |
| Forced IPC  | Always go through the daemon; error if the socket is missing | `--ipc <path>`   |

The default socket lives at `~/Library/Application Support/nagori/nagori.sock`.

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
Cmd+V into the frontmost app (macOS only).

### `nagori clear [--older-than-days N]`

Soft-delete unpinned entries. With no flag, deletes everything that is not
pinned. With `--older-than-days`, deletes entries created before that cutoff.

### `nagori ai <action> <id>`

Run a local AI action against an entry. Available actions: `summarize`,
`translate`, `format-json`, `format-markdown`, `explain-code`, `rewrite`,
`extract-tasks`, `redact-secrets`. `Secret` entries are blocked unless the
action is `redact-secrets`; `Private` entries are redacted before being sent.

### `nagori doctor`

Print database/socket paths, capture and AI flags, and macOS permission
status.

### `nagori daemon run [--capture-interval-ms N] [--maintenance-interval-min N]`

Boot the daemon. Holds the SQLite handle, runs the capture loop, and serves
the Unix socket.

### `nagori daemon stop`

Send a shutdown request via IPC. Requires `--ipc <socket>`.

### `nagori daemon status`

Print socket and DB metadata without contacting a daemon.

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

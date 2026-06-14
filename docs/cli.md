# `nagori` CLI

The CLI is a single binary, `nagori`, that can either talk directly to the
SQLite database or to a running nagori over its IPC endpoint (Unix-domain
socket on macOS / Linux, Win32 named pipe on Windows). Both the desktop
app and the headless `nagori daemon run` serve that endpoint, so the CLI
works the same whichever one is running.

## Installation

The `nagori` binary ships **inside the desktop app bundle** rather than as a
separate download — there is one artifact to install and update.

* **Homebrew (macOS).** `brew install --cask mhiro2/tap/nagori` links the
  bundled binary onto Homebrew's PATH automatically (via the cask `binary`
  stanza), so `nagori` is usable from a terminal as soon as the cask is
  installed.
* **Manual / direct install (macOS or Linux).** Apps launched from a file
  manager don't put the bundled binary on your PATH. Open
  **Settings → CLI → Command-line tool** and click **Install nagori CLI**: it
  symlinks the bundled binary into `~/.local/bin` (no admin prompt). If that
  directory isn't on your PATH yet, the dialog shows the line to add to your
  shell profile:

  ```sh
  export PATH="$HOME/.local/bin:$PATH"
  ```

  The installer links against the binary inside the installed app, so the app
  must live in a stable location. If you launch it straight from the `.dmg` or
  an unmoved download (where macOS *translocates* the app to a temporary copy),
  or from a Linux AppImage's transient mount, the installer refuses rather than
  create a link that would break on quit — move the app to Applications (or use
  the `.deb`) and relaunch first.

* **Windows.** One-click install isn't wired up; copy the bundled `nagori.exe`
  from inside the app to a directory already on your PATH.

## Modes

| Mode        | When to use                                                  | Flag                |
|-------------|--------------------------------------------------------------|---------------------|
| Direct DB   | Default for reads. Opens the local SQLite history file       |  *(none)*           |
| Auto IPC    | Try the IPC endpoint; fall back to direct DB if unreachable  | `--auto-ipc`        |
| Forced IPC  | Always go through IPC; error if the endpoint is missing      | `--ipc <endpoint>`  |

Write commands (`add`, `copy`, `paste`, `pin`, `unpin`, `delete`,
`clear`) route by the single-instance lock the desktop app and the
daemon hold for their lifetime, decided once per invocation: when the
lock is free, nothing owns the store and the CLI writes the local DB
directly; when it is held, the write goes through the owner's IPC
endpoint so its search cache is invalidated and the palette updates —
a direct write can never desync a running instance:

| What's running                      | Where the write goes                       |
|-------------------------------------|--------------------------------------------|
| Desktop app or daemon, IPC on       | IPC (palette updates immediately)          |
| Nothing                             | Direct DB write (instance lock acquired)   |
| Instance running, IPC toggle off    | Refused — enable **Settings → CLI** or quit |

`--db <path>` follows the same rule: reads are always allowed, but a
write with `--db` is refused while a running instance owns that DB.

Defaults:

* **macOS** — DB at `~/Library/Application Support/nagori/nagori.sqlite`,
  socket at `~/Library/Application Support/nagori/nagori.sock`.
* **Windows** — DB under `%LOCALAPPDATA%\nagori\nagori.sqlite`, named pipe
  at `\\.\pipe\nagori`. Pass `--ipc \\.\pipe\<name>` to override.

Set `NAGORI_DB_PATH=/path/to/nagori.sqlite` to redirect the store away
from the platform default. The desktop shell honours the same variable,
so both processes target the same DB when launched with it. Note the
IPC endpoint does **not** move with the variable: the desktop always
serves the default endpoint, so whichever process owns it serves the
CLI. To address two instances deterministically, start the daemon with
a custom `--ipc <endpoint>` and pass the same flag to the CLI; the
desktop then owns the default endpoint uncontended.

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

### `nagori clear (--all | --older-than-days N)`

Hard-delete unpinned entries (the row and its representations, blobs,
embeddings, and search index are physically removed via cascade — unlike
`nagori delete`, which is soft). One scope flag is required: `--all`
deletes every unpinned entry, while `--older-than-days N` deletes only
unpinned entries created before that cutoff. A bare `nagori clear` with no
flag is rejected at parse time so the command can't wipe history by
accident.

### `nagori quick <action> <id>`

Run a deterministic on-device quick action against an entry. These never touch a
language model and are always available regardless of the AI provider
configuration. Actions: `format-json`, `extract-tasks`, `redact-secrets`,
`summarize-first-sentence`. `Secret` entries are blocked unless the action is
`redact-secrets`; `Private` entries are redacted first.

### `nagori ai <action> <id> [--to <lang>] [--from <lang>]`

Run a model-backed AI action against an entry, streaming the result by default
(`--no-stream` prints only the final text; `--json` / `--jsonl` select the
output shape). Actions: `summarize`, `translate`, `rewrite`, `format-markdown`,
`extract-tasks`, `explain-code` — all are wired on macOS; on other platforms
they report a capability mismatch since no engine is available. `Secret`
entries are blocked; `Private` entries are redacted before the model sees them.

`translate` requires `--to <lang>` (a BCP-47 / ISO code such as `ja`, `en`, or
`zh-Hans`); `--from <lang>` is optional and the source is auto-detected when
omitted, e.g. `nagori ai translate <id> --to ja`. On-device translation uses the
Apple Translation framework, which needs the desktop app-bundle runtime context,
so the headless CLI path is available but live translation is exercised in the
app.

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
- `ipc\tpanic_count=N\tpanics_last_5m=M\tmax_connections=<N|(unknown)>[\t<last panic>]`
  — `panic_count` is the total IPC handler tasks the accept loop has
  observed to panic over this daemon's lifetime; `panics_last_5m` is how
  many of those landed in the last five minutes (so a current panic loop
  is distinguishable from one stale fluke); `max_connections` is the
  active in-flight handler ceiling (`(unknown)` for a daemon that hasn't
  stamped it yet). The optional trailing field is the last panic message
  for one-glance triage. A non-zero count is a hint to grep the daemon
  log for the matching stack trace; it does not flip the top-level `ok`
  flag because a single panic does not have the same operational impact
  as a wedged retention loop.

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
Available on macOS, Windows, and Linux; other platforms exit with
"daemon run is only available on macOS, Windows, and Linux in this build".

`--capture-interval-ms` accepts `1`–`3600000` (default `500`) and
`--maintenance-interval-min` accepts `1`–`525600` (default `30`); `0` is
rejected at parse time so neither loop can be spun into a busy loop.

### `nagori daemon stop`

Send a shutdown request via IPC. Requires `--ipc <endpoint>`.

### `nagori daemon status`

Report status for the store the command can see. Without `--ipc` /
`--auto-ipc` the command reads the local DB directly and **does not
probe a daemon** — the output is prefixed `local (daemon not probed)`
(JSON: `"source": "local", "daemon_probed": false`) so a dead daemon is
never reported as healthy. With `--ipc <endpoint>` (or `--auto-ipc` when
the endpoint is reachable) the daemon itself answers and the output
reports `ok` plus its version (JSON: `"source": "daemon"`).

## Exit codes

The CLI returns the following exit codes. Codes `2`, `4`, `5`, `6`, `7`,
and `8` map onto the typed `AppError` variants returned from the
`nagori-core` services. Any error that cannot be classified as a typed
`AppError` is treated as an internal failure and also returns `8`.

| Code | Meaning            | Underlying `AppError`                                |
|------|--------------------|------------------------------------------------------|
| 0    | Success            | —                                                    |
| 2    | Invalid input      | `InvalidInput`                                       |
| 4    | Not found          | `NotFound`                                           |
| 5    | Policy violation   | `Policy`                                             |
| 6    | Permission denied  | `Permission`                                         |
| 7    | Unsupported        | `Unsupported` (e.g. unsupported `SearchMode`)        |
| 8    | Internal error     | `Storage`, `Search`, `Platform`, `Ai`, `Configuration`, or any unclassified error |

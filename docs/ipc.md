# IPC Protocol

Nagori exposes a per-platform stream transport: a Unix domain socket
on macOS / Linux and a Win32 named pipe on Windows. Each connection speaks a
newline-delimited JSON protocol — the client writes one JSON object per line
and reads exactly one response line back. The wire format is identical
across platforms; only the endpoint differs.

The server is hosted by whichever process owns the store: the headless
`nagori daemon run` *or* the desktop app, which runs the same IPC
supervisor (`nagori_daemon::spawn_cli_ipc_supervisor`) against its
in-process runtime. The two are mutually exclusive per store directory
(single-instance lock), serve byte-identical IPC, and honour the same
`cli_ipc_enabled` settings toggle, so everything below applies to both;
"daemon" in this document means the serving process. The desktop host is
fail-closed — it serves only after the persisted settings loaded — and
retries a failed bind with backoff instead of aborting the app.

Binding the endpoint is gated by a second lock keyed on the endpoint
itself (independent of the store lock). Even when `NAGORI_DB_PATH` /
`--db` let a custom-store process and a default-store process run at once,
only the endpoint-lock holder serves the default endpoint; the other is
refused and retries until the owner exits and releases it — a
deterministic hand-off rather than a race to reclaim the socket. See
ARCHITECTURE.md "Single-instance & stale-socket handling".

## Endpoint location

* **macOS** (default): `~/Library/Application Support/nagori/nagori.sock`.
  The bind path is created with a `0o077` umask and explicitly `chmod`ed to
  `0o600`, so only the daemon's user can `connect(2)` it.
* **Windows** (default): `\\.\pipe\nagori`. The pipe is created with an
  explicit DACL whose single ACE grants only the daemon's user
  (`GENERIC_READ | GENERIC_WRITE`); no other local user — even on the same
  desktop session — can open it. `reject_remote_clients(true)` additionally
  closes the UNC/SMB surface. The sibling token file is a second,
  independent gate on top of the pipe ACL.
* Override with `--ipc <endpoint>` on both the daemon and the CLI. On
  Windows pass the pipe name (e.g. `\\.\pipe\nagori-dev`).

The auth token is a 32-byte random hex string written to a token file:

* On Unix it is created with `0o600` mode (born under a `0o077` umask via an
  `O_CREAT|O_EXCL` temp file that is then `rename(2)`d into place, so it is
  never world-readable for an instant and a planted symlink is replaced, not
  followed). The token always lives inside the daemon's private `0o700`
  app-data directory: for the default endpoint that directory also holds the
  socket, but for a custom `--ipc` endpoint the token stays in the app-data
  directory rather than beside the socket — so a world-writable socket
  parent (e.g. `/tmp`) cannot be used to attack the token path.
* On Windows the token file lives under `%LOCALAPPDATA%\nagori\` and is
  written with an explicit DACL granting the current user,
  `BUILTIN\Administrators`, and `NT AUTHORITY\SYSTEM`. The DACL is forced
  onto every launch by creating a `CREATE_NEW` temp file with the descriptor
  attached and atomically `MoveFileExW`-ing it over any previous entry — a
  plain `CREATE_ALWAYS` write would truncate but inherit the stale
  descriptor.

When you launch the daemon with `--ipc <custom>`, the CLI and daemon both
derive the token filename from the endpoint using one scheme on every
platform. The default endpoint keeps the historic `nagori.token`; every
other endpoint gets `<sanitised>-<8 hex>.token`, where `<sanitised>` is the
endpoint's last path / pipe segment with filesystem-unsafe characters
replaced and the suffix is the first eight hex characters of
`SHA-256(<full endpoint>)`. So `…/dev.sock` becomes `dev.sock-<8 hex>.token`
and `\\.\pipe\nagori-dev` becomes `nagori-dev-<8 hex>.token`. Without the
hash, two endpoints that sanitise to the same segment (e.g. `\\.\pipe\a:b`
and `\\.\pipe\a?b`, both collapsing to `a_b`) would race for the same token
file. Every envelope is rejected unless its `token` matches via
constant-time comparison.

## Framing

```
<request-json>\n
<response-json>\n
```

Requests and responses are externally-tagged serde enums: a unit variant is a
plain string (`"Health"`), and a payload variant nests under the variant name
(`{"Search": { ... }}`). Field names in payloads are `snake_case`.

## Request kinds

```jsonc
{ "Search":         { "query": "kubectl", "limit": 50 } }
{ "ListRecent":     { "limit": 50, "include_sensitive": false } }
{ "ListPinned":     { "include_sensitive": false } }
{ "GetEntry":       { "id": "<uuid>", "include_sensitive": false } }
{ "AddEntry":       { "text": "hello" } }
{ "CopyEntry":      { "id": "<uuid>" } }
{ "PasteEntry":     { "id": "<uuid>" } }
{ "PasteEntry":     { "id": "<uuid>", "format": "plain_text" } }
{ "DeleteEntry":    { "id": "<uuid>" } }
{ "PinEntry":       { "id": "<uuid>", "pinned": true } }
{ "RunQuickAction": { "id": "<uuid>", "action": "FormatJson" } }
{ "RunAiAction":    { "id": "<uuid>", "action": "Summarize" } }
{ "RunAiAction":    { "id": "<uuid>", "action": "Translate",
                      "options": { "target_language": "ja", "source_language": "en" } } }
"GetSettings"
{ "UpdateSettings": { "value": { /* AppSettings */ } } }
{ "UpdateSettings": { "value": { /* AppSettings */ }, "expected_revision": 7 } }
{ "Clear":          "All" }
{ "Clear":          { "OlderThanDays": { "days": 30 } } }
"Doctor"
"Health"
"Capabilities"
"Shutdown"
```

`PasteEntry.format` is optional (`PasteFormat`, `"preserve"` / `"plain_text"`);
omit it to paste with the entry's preserved formatting. `RunQuickAction` runs a
deterministic on-device transform (`"FormatJson"`, `"ExtractTasks"`,
`"RedactSecrets"`, `"SummarizeFirstSentence"`) — distinct from the model-backed
`RunAiAction`. `Clear` is an externally-tagged enum: `"All"` wipes every
unpinned entry, `{"OlderThanDays":{"days":N}}` wipes unpinned entries older than
`N` days.

`RunAiAction.options` is optional (`AiRequestOptions`, defaulted when absent).
It carries per-request overrides — `translate`'s `target_language` /
`source_language`, plus tightening-only caps (`timeout_ms`, `max_input_tokens`,
`max_output_tokens`, `streaming`) the daemon clamps against the AI settings
before dispatch. Without it `translate` over IPC would run with no target
language and fail.

`UpdateSettings` over IPC is last-writer-wins by default: omit
`expected_revision` and the full blob is persisted unconditionally. Supplying
`expected_revision` (the `revision` from a prior `GetSettings` response) routes
the write through the compare-and-swap save, so it is rejected with
`settings_conflict` when the stored revision has moved under the loaded
snapshot — the protection a client needs when a concurrent single-field change
(e.g. a tray capture toggle) could otherwise be reverted by a stale blob. The
CLI has no settings-write command, and the desktop settings window goes through
the in-process runtime (`save_settings_checked`) rather than this socket — see
ARCHITECTURE.md "Optimistic concurrency on settings writes".

## Response kinds

```jsonc
{ "Search":   { "results": [/* SearchResultDto */] } }
{ "Entries":  [/* EntryDto */] }
{ "Entry":    { /* EntryDto */ } }
{ "Settings": { "value": { /* AppSettings */ }, "revision": 7 } }
{ "AiOutput": { /* AiOutputDto */ } }
{ "Cleared":  { "deleted": 12 } }
{ "Doctor":       { /* DoctorReport */ } }
{ "Health":       { "ok": true, "version": "0.0.0" } }
{ "Capabilities": { /* PlatformCapabilities */ } }
"Ack"
{ "Error":    { "code": "not_found", "message": "...", "recoverable": false } }
```

## Error codes

| Code                 | Meaning                                                                                          |
|----------------------|--------------------------------------------------------------------------------------------------|
| `storage_error`      | SQLite or filesystem failure                                                                     |
| `search_error`       | Indexing / ranker failure                                                                        |
| `platform_error`     | OS API failure (clipboard read/write, paste synthesis)                                           |
| `permission_error`   | Platform permission missing or denied (macOS TCC, etc.)                                          |
| `ai_error`           | AI provider failed                                                                               |
| `policy_error`       | Capture/output policy refused the request                                                        |
| `not_found`          | Entry id missing or already deleted                                                              |
| `invalid_input`      | Malformed JSON, bad UUID, conflicting flags                                                      |
| `unsupported`        | Feature not available on this platform                                                           |
| `configuration_error`| Runtime built with a missing critical adapter — a build defect, not a user-recoverable failure   |
| `unauthorized`       | Envelope's `token` field did not match the daemon's `nagori.token`                               |
| `invalid_request`    | Transport-level rejection: timed-out read, request exceeded `MAX_IPC_BYTES`, or unparseable JSON |
| `response_too_large` | Handler produced a response exceeding `MAX_IPC_BYTES` after serialisation                        |
| `deadline_exceeded`  | Handler ran past the server-side handler deadline (a wedged handler backstop) and was force-released |

`recoverable=false` is set for `not_found`, `policy_error`,
`configuration_error`, `unauthorized`, `response_too_large`, and
`deadline_exceeded`; everything else is treated as transient by the CLI.

When `cli_ipc_enabled` is switched off, the daemon drains the active IPC
server and removes the socket/token files. While disabled, only `Health`,
`Doctor`, `Capabilities`, and `Shutdown` are accepted by the runtime; other
requests return `permission_error` if they reach an already-authenticated
in-flight handler.

## Example session

Each connection carries exactly one request and one response — the
server reads a single newline-terminated envelope, writes the response
line, and closes the connection. To issue multiple commands, open a new
connection per command. (`nc -U` reflects this: every `Health` /
`ListRecent` below would in practice be a separate `nc` invocation.)

A client **must keep the connection fully open until it has read the
response** — it must not `shutdown(Write)` (half-close) its write half after
sending the request. While a handler runs, the server watches the connection's
read half for EOF as a *peer gave up* signal and cancels the handler (freeing
its connection slot, any AI permit, and in-flight DB work) the moment it sees
one. A half-close looks identical to a disconnect, so half-closing while still
waiting for the response would have the server cancel a request the client is
in fact still waiting on. The bundled `nagori` CLI follows this contract.

macOS / Linux:

```
$ nc -U ~/Library/Application\ Support/nagori/nagori.sock
{"token":"<hex>","request":"Health"}
{"Health":{"ok":true,"version":"0.0.0"}}
```

```
$ nc -U ~/Library/Application\ Support/nagori/nagori.sock
{"token":"<hex>","request":{"ListRecent":{"limit":3,"include_sensitive":false}}}
{"Entries":[ ... ]}
```

Windows (PowerShell, via `NamedPipeClientStream`): connect to
`\\.\pipe\nagori`, write one envelope line, read one response line, then
close. The `nagori` CLI handles this transparently — `nagori --auto-ipc
...` and `nagori daemon status` pick the right transport for the host
and open a fresh connection per command.

## Concurrency

The daemon spawns one Tokio task per accepted connection. The same
`NagoriRuntime` instance is shared, so requests are linearised at the SQLite
mutex but can otherwise overlap.

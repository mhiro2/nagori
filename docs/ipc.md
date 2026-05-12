# IPC Protocol

`nagori-daemon` exposes a per-platform stream transport: a Unix domain socket
on macOS / Linux and a Win32 named pipe on Windows. Each connection speaks a
newline-delimited JSON protocol — the client writes one JSON object per line
and reads exactly one response line back. The wire format is identical
across platforms; only the endpoint differs.

## Endpoint location

* **macOS** (default): `~/Library/Application Support/nagori/nagori.sock`.
  The bind path is created with a `0o077` umask and explicitly `chmod`ed to
  `0o600`, so only the daemon's user can `connect(2)` it.
* **Windows** (default): `\\.\pipe\nagori`. The pipe is created with the
  default named-pipe security descriptor inherited from the daemon process
  — there is no custom DACL yet. Authentication is enforced via the
  sibling token file rather than the pipe ACL.
* Override with `--ipc <endpoint>` on both the daemon and the CLI. On
  Windows pass the pipe name (e.g. `\\.\pipe\nagori-dev`).

The auth token is a 32-byte random hex string written to a sibling file:

* On Unix, `nagori.token` lives next to the socket and is created with
  `0o600` mode, so only the daemon's user can read it.
* On Windows, the token file lives under `%LOCALAPPDATA%\nagori\` with
  the default NTFS permissions inherited from the per-user directory.
  There is no custom DACL on either the pipe or the token file yet.

When you launch the daemon with `--ipc <custom>`, the CLI and daemon both
derive the token filename from the endpoint. On Unix the derivation uses
the socket stem (e.g. `dev.token` for `…/dev.sock`). On Windows the
default pipe `\\.\pipe\nagori` keeps the historic filename
`nagori.token`; every other pipe gets `<sanitised>-<8 hex>.token` where
the suffix is the first eight hex characters of `SHA-256(pipe name)` —
without the hash, two pipe names that sanitise to the same segment (e.g.
`\\.\pipe\a:b` and `\\.\pipe\a?b` both collapse to `a_b`) would race for
the same token file. Every envelope is rejected unless its `token`
matches via constant-time comparison.

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
{ "DeleteEntry":    { "id": "<uuid>" } }
{ "PinEntry":       { "id": "<uuid>", "pinned": true } }
{ "RunAiAction":    { "id": "<uuid>", "action": "Summarize" } }
"GetSettings"
{ "UpdateSettings": { "value": { /* AppSettings */ } } }
{ "Clear":          { "older_than_days": 30 } }
"Doctor"
"Health"
"Shutdown"
```

## Response kinds

```jsonc
{ "Search":   { "results": [/* SearchResultDto */] } }
{ "Entries":  [/* EntryDto */] }
{ "Entry":    { /* EntryDto */ } }
{ "Settings": { /* AppSettings */ } }
{ "AiOutput": { /* AiOutputDto */ } }
{ "Cleared":  { "deleted": 12 } }
{ "Doctor":   { /* DoctorReport */ } }
{ "Health":   { "ok": true, "version": "0.0.0" } }
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
| `unauthorized`       | Envelope's `token` field did not match the daemon's `nagori.token`                               |
| `invalid_request`    | Transport-level rejection: timed-out read, request exceeded `MAX_IPC_BYTES`, or unparseable JSON |
| `response_too_large` | Handler produced a response exceeding `MAX_IPC_BYTES` after serialisation                        |

`recoverable=false` is set for `not_found`, `policy_error`, `unauthorized`,
and `response_too_large`; everything else is treated as transient by the CLI.

When `cli_ipc_enabled` is switched off, the daemon drains the active IPC
server and removes the socket/token files. While disabled, only `Health`,
`Doctor`, and `Shutdown` are accepted by the runtime; other requests return
`permission_error` if they reach an already-authenticated in-flight handler.

## Example session

Each connection carries exactly one request and one response — the
server reads a single newline-terminated envelope, writes the response
line, and closes the connection. To issue multiple commands, open a new
connection per command. (`nc -U` reflects this: every `Health` /
`ListRecent` below would in practice be a separate `nc` invocation.)

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

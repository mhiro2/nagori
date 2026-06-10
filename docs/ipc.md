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
{ "RunAiAction":    { "id": "<uuid>", "action": "Translate",
                      "options": { "target_language": "ja", "source_language": "en" } } }
"GetSettings"
{ "UpdateSettings": { "value": { /* AppSettings */ } } }
{ "Clear":          { "older_than_days": 30 } }
"Doctor"
"Health"
"Capabilities"
"Shutdown"
```

`RunAiAction.options` is optional (`AiRequestOptions`, defaulted when absent).
It carries per-request overrides — `translate`'s `target_language` /
`source_language`, plus tightening-only caps (`timeout_ms`, `max_input_tokens`,
`max_output_tokens`, `streaming`) the daemon clamps against the AI settings
before dispatch. Without it `translate` over IPC would run with no target
language and fail.

`UpdateSettings` over IPC is a last-writer-wins write: it carries no revision
token and is not compare-and-swap checked. The CLI has no settings-write
command, so the only full-blob writer that needs lost-update protection is the
desktop settings window, which goes through the in-process runtime
(`save_settings_checked`) rather than this socket — see ARCHITECTURE.md
"Optimistic concurrency on settings writes".

## Response kinds

```jsonc
{ "Search":   { "results": [/* SearchResultDto */] } }
{ "Entries":  [/* EntryDto */] }
{ "Entry":    { /* EntryDto */ } }
{ "Settings": { /* AppSettings */ } }
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

# IPC Protocol

`nagori-daemon` exposes a Unix domain socket. Each connection speaks a
newline-delimited JSON protocol: the client writes one JSON object per line
and reads exactly one response line back.

## Socket location

* Default: `~/Library/Application Support/nagori/nagori.sock` (macOS).
* Override with `--ipc <path>` on both the daemon and the CLI.

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
"ListPinned"
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

| Code              | Meaning                                                           |
|-------------------|-------------------------------------------------------------------|
| `storage_error`   | SQLite or filesystem failure                                      |
| `search_error`    | Indexing / ranker failure                                         |
| `platform_error`  | OS API failure (clipboard read/write, paste synthesis)            |
| `permission_error`| macOS permission missing or denied                                |
| `ai_error`        | AI provider failed                                                |
| `policy_error`    | Capture/output policy refused the request                         |
| `not_found`       | Entry id missing or already deleted                               |
| `invalid_input`   | Malformed JSON, bad UUID, conflicting flags                       |
| `unsupported`     | Feature not available on this platform                            |

`recoverable=false` is set for `not_found` and `policy_error`; everything else
is treated as transient by the CLI.

## Example session

```
$ nc -U ~/Library/Application\ Support/nagori/nagori.sock
"Health"
{"Health":{"ok":true,"version":"0.0.0"}}
{"ListRecent":{"limit":3,"include_sensitive":false}}
{"Entries":[ ... ]}
```

## Concurrency

The daemon spawns one Tokio task per accepted connection. The same
`NagoriRuntime` instance is shared, so requests are linearised at the SQLite
mutex but can otherwise overlap.

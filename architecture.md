# rocqd Architecture

rocqd is a caching daemon for Rocq (formerly Coq) compilation. It drives `vsrocqtop` — the VsRocq language server — via LSP over stdio, providing incremental checking and query support from the command line.

## Module Overview

```
src/
├── main.rs          CLI entry point (clap subcommands)
├── lib.rs           Public module declarations
├── daemon.rs        Unix socket server, request dispatch
├── client.rs        CLI client for connecting to the daemon
├── session.rs       File session: owns vsrocqtop, tracks state
├── protocol.rs      rocqd's own JSON-RPC protocol types
└── lsp/
    ├── mod.rs
    ├── transport.rs  Content-Length framing over async I/O
    ├── jsonrpc.rs    JSON-RPC 2.0 message types
    └── vsrocq.rs     VsRocq client: spawns/drives vsrocqtop
```

## Data Flow

### Compilation

```
CLI (rocqd compile foo.v)
  → Unix socket → daemon.rs::handle_compile
    → FileSession::open (or recompile if same file)
      → VsRocqClient::spawn (forks vsrocqtop)
      → initialize + initialized + didOpen + interpretToEnd
      → drain_events: wait for processedRange to cover document
        OR error diagnostics / blockOnError
    ← diagnostics + success/failure
  ← JSON-RPC response
← exit code 0 (success) or 1 (errors)
```

### Queries

```
CLI (rocqd query foo.v:5 "Check nat.")
  → Unix socket → daemon.rs::handle_query
    → parse_query_text ("Check nat." → type=check, pattern=nat)
    → FileSession::query
      → VsRocqClient::check (sends prover/check, waits for response)
    ← query result string
  ← JSON-RPC response
← printed to stdout
```

## Key Components

### LspTransport (`lsp/transport.rs`)

Implements Content-Length framing for LSP messages over any async reader/writer pair. Handles `Content-Length: N\r\n\r\n{json}` encoding/decoding. Used for both vsrocqtop communication (via stdio pipes) and can be adapted for other transports.

### JSON-RPC (`lsp/jsonrpc.rs`)

Standard JSON-RPC 2.0 types: `Request`, `Response`, `Notification`, `Message` enum. The `Message::parse()` method distinguishes message types by their fields (presence/absence of `id` and `method`).

### VsRocqClient (`lsp/vsrocq.rs`)

Spawns a `vsrocqtop` child process and communicates via LSP. Uses direct I/O (no background tasks or channels) — the caller drives reading and writing.

Key methods:
- `initialize()` — LSP handshake (initialize request + initialized notification)
- `did_open()` / `did_change()` — document sync
- `interpret_to_end(uri, version)` — trigger full file interpretation (the `version` field is required by vsrocqtop)
- `recv_event()` — reads messages, parses notifications into `VsRocqEvent`s, skips responses
- `check()` / `print()` / `about()` — send query requests, wait for matching response

VsRocq-specific notifications (from vsrocqtop → client):
- `prover/updateHighlights` — progress: prepared/processing/processed ranges
- `textDocument/publishDiagnostics` — error/warning diagnostics
- `prover/blockOnError` — processing stopped at error (may not always be sent)

### FileSession (`session.rs`)

Manages one `.v` file's lifecycle with vsrocqtop. Tracks file path, content, SHA-256 hash (for change detection), document version, diagnostics, and execution status.

Completion detection (`drain_events`):
1. Monitor `prover/updateHighlights` notifications for `processedRange` covering the entire document (both line number AND character position on the last line)
2. If error diagnostics arrive (severity = ERROR), immediately report as `BlockedOnError`
3. If `prover/blockOnError` notification arrives, report as `BlockedOnError`

### Daemon (`daemon.rs`)

Unix socket server listening at `$XDG_RUNTIME_DIR/rocqd.sock` (or `/tmp/rocqd-$UID.sock`). Each client connection sends one newline-delimited JSON-RPC request and receives one response.

Maintains a single `FileSession` (Phase 1). If a different file is compiled, the previous session is evicted. Handles: compile, query, status, shutdown, invalidate.

### Client (`client.rs`)

CLI functions that connect to the daemon socket, send JSON-RPC requests, and format output. The `compile` command prints diagnostics in `file:line:col: severity: message` format and exits with code 1 on errors.

## Protocol

rocqd's own protocol (daemon ↔ CLI client) uses newline-delimited JSON-RPC over Unix sockets:

| Method       | Request                    | Response                        |
|-------------|----------------------------|---------------------------------|
| `compile`   | `{file, flags}`           | `{diagnostics, success}`       |
| `query`     | `{file, line, text}`      | `{response}`                   |
| `status`    | `{}`                      | `{sessions: [{file, status}]}` |
| `shutdown`  | `{}`                      | `{}`                           |
| `invalidate`| `{file}`                  | `{}`                           |

## VsRocq Protocol Notes

vsrocqtop uses the `prover/` method prefix (not `vsrocq/` as some documentation suggests):
- `prover/interpretToEnd` — requires `textDocument.version` field
- `prover/updateHighlights` — uses camelCase field names (processedRange, etc.)
- `prover/blockOnError` — may not fire for all errors; diagnostics are more reliable
- `prover/check`, `prover/print`, `prover/about` — query requests

vsrocqtop's stderr should be discarded (piped to null) to prevent pipe buffer deadlocks.

## Phase 2 Considerations

Current limitations that Phase 2 should address:
- Single file session (daemon evicts previous session on new file)
- No multi-file project support (load path, dependencies)
- No incremental recompilation (full re-send on change)
- Fixed 100ms delays between LSP messages (could be tuned or removed)

# Design Document: `rocqd` — A Caching Daemon for Rocq Compilation

See [notes.md](notes.md) for background research on VsRocq internals, the Rocq OCaml API, and investigation results on VsRocq's protocol capabilities.

## Motivation

Rocq compilation is slow. Running `rocq compile foo.v` starts a fresh Rocq process, loads all dependencies from `.vo` files, processes every sentence from scratch, and exits. If you change one tactic in the middle of a file, the entire prefix must be re-executed. If you add a `Check` or `Print` query to inspect intermediate state, the file must be re-compiled from scratch.

The interactive IDE story (VsRocq) solves this for editor use — it maintains a persistent Rocq process, tracks sentence boundaries, and navigates forward/back as the user edits. But this machinery is locked behind an LSP server coupled to VS Code. There is no way to get these benefits from the command line.

`rocqd` is a daemon that brings incremental, cached Rocq compilation to the command line. The interface is simply `rocq compile`, but under the hood, persistent Rocq instances are maintained, state is re-used by navigating forward and back in response to source changes, and queries can be injected from the command line without losing any compilation state.

## Language Choice

There are two viable architectures, with different language implications:

### Option A: OCaml — Direct Rocq API

The daemon links against Rocq's OCaml API (`Vernac.process_expr`, state types, parser, Summary system). This gives full control over state caching, sentence-level diffing, and query injection. The daemon is an OCaml program that happens to also run a socket server. This is the architecture described in the "Direct API Design" section below.

### Option B: Rust (or any language) — Driving VsRocq via LSP

Instead of linking Rocq directly, the daemon manages `vsrocqtop` child processes and speaks LSP to them. VsRocq already handles parsing, incremental re-execution, and state management internally. The daemon becomes a multiplexing LSP client that translates its simple JSON-RPC protocol into LSP messages.

This works because VsRocq's custom protocol covers the needed operations:
- `textDocument/didOpen` + `didChange` → VsRocq handles parsing, diffing, and incremental re-execution
- `prover/interpretToEnd` → trigger compilation
- `prover/check`, `prover/print`, `prover/about`, `prover/locate`, `prover/search` → query execution
- Standard LSP `textDocument/publishDiagnostics` → errors/warnings
- `prover/updateHighlights` → execution progress

**Tradeoffs:**

| | OCaml (direct API) | Rust over LSP |
|---|---|---|
| State caching | Full control, in-process | Implicit — states live in vsrocqtop |
| Query injection | Custom transparent handling | Use VsRocq's query commands |
| Incremental recompile | Implement the diff ourselves | VsRocq does it internally |
| `.vo` generation | Call `Library.save` directly | Not supported — need separate `rocq compile` pass |
| Proof delegation | Implement ourselves | VsRocq has it built in |
| Sentence parsing | Use Rocq's parser directly | Not fully exposed via protocol (see notes.md) |
| Customizability | Unlimited | Limited to LSP protocol surface |
| Development speed | Slower (must understand Rocq internals) | Faster (VsRocq is a black box) |

### Recommendation

Option B (Rust over LSP) is the faster path to a working prototype. The main gaps are `.vo` generation and sentence boundary visibility (see notes.md for investigation details). For batch compilation that must produce `.vo` files, a final `rocq compile` pass is needed regardless. The daemon's value is in fast incremental *checking* and query support.

## Design

### Overview

```
┌──────────────────────────────────────────────┐
│                   Client                      │
│  `rocq compile` / `rocqd query` / editor      │
└──────────────┬───────────────────────────────┘
               │ Unix socket / JSON-RPC
┌──────────────▼───────────────────────────────┐
│              rocqd  (daemon process)           │
│                                               │
│  ┌─────────────────────────────────────────┐  │
│  │          Request Router                  │  │
│  └────────┬──────────────┬─────────────────┘  │
│           │              │                     │
│  ┌────────▼──────┐ ┌────▼──────────────┐     │
│  │  File Session │ │  File Session      │     │
│  │  (foo.v)      │ │  (bar.v)           │     │
│  │               │ │                    │     │
│  │  vsrocqtop    │ │  vsrocqtop         │     │
│  │  (child proc) │ │  (child proc)      │     │
│  └───────────────┘ └────────────────────┘     │
└───────────────────────────────────────────────┘
```

### Component: File Session

A File Session manages one `.v` file. It owns a `vsrocqtop` child process and translates rocqd requests into LSP messages. A session maintains:

**Document content.** The last file content sent to `vsrocqtop` via `didOpen`/`didChange`. Used to compute minimal `didChange` edits on recompilation.

**Execution status.** Tracked via `prover/updateHighlights` notifications from `vsrocqtop`: which ranges are processed, processing, or prepared.

**Diagnostics.** Accumulated from `textDocument/publishDiagnostics` notifications.

**File content hash.** Used for quick staleness checks — if the file hasn't changed, skip re-sending.

#### Session Lifecycle

1. **Open**: Client requests compilation of `foo.v`. The daemon reads the file, sends `textDocument/didOpen` to `vsrocqtop`, then sends `prover/interpretToEnd` to trigger full checking.

2. **Check**: `vsrocqtop` processes sentences sequentially, sending `prover/updateHighlights` as progress and `textDocument/publishDiagnostics` for errors/warnings. The session accumulates these until checking completes (all ranges are in `processedRange`, or a `prover/blockOnError` is received).

3. **Re-use on change**: When `foo.v` is compiled again after an edit, the daemon re-reads the file, computes a diff against the last sent content, and sends `textDocument/didChange` with the edits. VsRocq internally handles sentence-level diffing, prefix reuse, and selective re-execution. The daemon then sends `prover/interpretToEnd` again.

4. **Query**: For `rocqd query foo.v:42 "Check nat."`, the daemon sends `prover/check { textDocument, position: {line: 42}, pattern: "nat" }` to `vsrocqtop` and returns the response.

5. **Evict**: Sessions are evicted under memory pressure. Eviction kills the `vsrocqtop` child process. On the next compile request for that file, a new session and process are created.

### Component: Query Support

Queries use VsRocq's custom request protocol:

- `prover/check { textDocument, position, pattern }` → `Check` command
- `prover/print { textDocument, position, pattern }` → `Print` command
- `prover/about { textDocument, position, pattern }` → `About` command
- `prover/locate { textDocument, position, pattern }` → `Locate` command
- `prover/search { textDocument, position, pattern }` → `Search` command

These execute against the Rocq state at the given position without modifying any state. The file must have been compiled (or at least checked up to that position) first.

**Transparent query handling.** When the source file contains `Check`/`Print`/etc. commands inline, VsRocq treats them as regular sentences. Unlike the direct-API approach (where queries can be stripped from the diff to avoid invalidation), the LSP approach simply lets VsRocq handle them normally. The incremental re-execution means adding a query only causes re-execution from that point, and since queries don't change the proof state, subsequent sentences re-execute identically.

**Query-only mode.** `rocqd query foo.v:42 "Check nat."` uses the `prover/check` request against an already-compiled file's session. This is fast — it reads existing state without any re-execution.

### Component: Request Router

The daemon listens on a Unix domain socket (default `$XDG_RUNTIME_DIR/rocqd.sock`) and handles JSON-RPC requests. The protocol is intentionally minimal:

```
// Compile a file. Returns diagnostics (errors, warnings).
// The daemon caches state for future re-use.
compile(file: string, flags: string[]) → {
  diagnostics: Diagnostic[],
}

// Execute a query against the state at a position in a compiled file.
// Does not modify any cached state.
query(file: string, line: int, text: string) → {
  response: string
}

// Invalidate cached state for a file (e.g., after a dependency changes).
invalidate(file: string) → {}

// Shut down the daemon.
shutdown() → {}

// Status/debug information.
status() → {
  sessions: { file: string, memory_mb: int }[],
  memory_total_mb: int
}
```

### Component: CLI Wrapper

The `rocq compile` command (or a wrapper script) transparently routes through the daemon:

```bash
# Without daemon (current behavior):
rocq compile foo.v          # starts Rocq, compiles from scratch, exits

# With daemon (proposed):
rocqd start                  # starts daemon in background
rocq compile foo.v          # daemon checks via vsrocqtop, caches state
# ... user edits foo.v ...
rocq compile foo.v          # daemon re-uses prefix, re-checks only changes
rocqd query foo.v:42 "Check nat."  # query without recompilation
rocqd stop                   # shuts down daemon
```

Implementation options for transparent interception:
1. **Wrapper script**: `rocq-cached` that checks for a running daemon and falls through to `rocq compile` if none exists.
2. **Environment variable**: `ROCQ_DAEMON=1 rocq compile foo.v` routes through daemon.
3. **rocq plugin**: Modify `rocq compile` upstream to optionally connect to a daemon.

Option 1 is simplest for an initial implementation. Option 3 is the long-term goal, potentially coordinating with the SOCCA refactoring effort.

### Handling Dependencies

A `.v` file typically begins with `From Foo Require Import Bar.` These `Require` commands load `.vo` files (pre-compiled libraries). The daemon must handle dependency changes:

**On first compile.** The `Require` sentences are executed normally by `vsrocqtop`, loading `.vo` files.

**When a dependency changes.** If `bar.vo` is recompiled, the daemon must invalidate sessions that depend on it. Detection options:
- Watch `.vo` file mtimes.
- On the next `compile` request, compare `.vo` mtimes against the session's creation time.
- If stale, kill the `vsrocqtop` process and start a fresh session.

### Memory Management

Each `vsrocqtop` process is memory-hungry. A single file compilation can consume hundreds of MB or several GB for large developments. The daemon must be aggressive about memory:

**Per-session tracking.** Monitor child process RSS via `/proc/[pid]/status` or equivalent.

**LRU eviction.** When total memory exceeds the limit (configurable, default 4 GB), kill the least-recently-used session's `vsrocqtop` process.

**Session limits.** Cap the maximum number of concurrent `vsrocqtop` processes.

### .vo Generation

VsRocq does not support `.vo` generation (see notes.md). For workflows that need `.vo` output (e.g., building libraries that other files depend on), the daemon can:

1. **Check-then-compile**: Use `vsrocqtop` for fast incremental checking during development. When the user needs a `.vo` (e.g., before committing or for downstream dependencies), run a standard `rocq compile` pass. Since the file is already known to be correct, this is a "production build" step.

2. **Parallel check+compile**: For files that are both being edited and depended upon, the daemon could run `rocq compile` in the background after a successful check, producing the `.vo` asynchronously.

## Implementation Plan

### Phase 1: Single-file daemon with VsRocq backend

- Rust binary with Unix socket listener, JSON-RPC protocol.
- Spawn and manage a single `vsrocqtop` child process.
- LSP client implementation for the VsRocq protocol subset.
- Single file session: didOpen, didChange, interpretToEnd.
- Collect diagnostics and report to caller.
- Query support via prover/check, prover/print, etc.
- CLI wrapper script that exposes compilation and queries.

### Phase 2: Multi-file sessions and dependency tracking

- Support multiple concurrent file sessions (multiple `vsrocqtop` processes).
- Track `.vo` mtimes for dependency invalidation.
- Parse `_CoqProject` for load paths (passed as `vsrocqtop` arguments).
- LRU eviction under memory pressure.

### Phase 3: .vo generation integration

- Check-then-compile workflow.
- Background `rocq compile` after successful check.

## Key Risks and Open Questions

**VsRocq as a black box.** We depend on VsRocq's correctness and performance for incremental re-execution. If VsRocq has bugs or performance issues, we inherit them and have limited ability to work around them. Mitigation: VsRocq is actively maintained and used by many Rocq developers.

**No .vo generation.** The daemon cannot produce `.vo` files through VsRocq. This means a separate `rocq compile` pass is needed for artifact generation. For pure "check my file" workflows this is fine; for full build system integration it adds complexity.

**LSP protocol surface.** We are limited to what VsRocq exposes. If we need functionality not in the protocol (e.g., fine-grained sentence boundaries, custom state inspection), we must contribute upstream or fork. The SOCCA effort may expand the available API.

**Process overhead.** Each file session requires a separate `vsrocqtop` OS process. This is heavier than in-process state management (the OCaml approach). Memory overhead is higher due to duplicated runtime state across processes.

**Coexistence with VsRocq.** If a user has both VsRocq (in their editor) and `rocqd` running, they manage independent Rocq instances. There is no state sharing. This is fine but wasteful. Long-term, VsRocq and `rocqd` could share a single daemon process — this aligns with the SOCCA vision of separating the language server core from the LSP transport.

**Race conditions.** File changes during compilation, concurrent compile requests for the same file, etc. The daemon should serialize requests per file (one at a time per session) and use file content hashing (not mtimes) for staleness detection.

**Continuous vs Manual mode.** VsRocq defaults to Manual mode. For batch compilation, Continuous mode may be more appropriate (automatically check the full document on open/change). Need to test which mode works better for our use case, or whether `interpretToEnd` in Manual mode is equivalent.

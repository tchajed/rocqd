# Design Document: `rocqd` — A Caching Daemon for Rocq Compilation

## Motivation

Rocq compilation is slow. Running `rocq compile foo.v` starts a fresh Rocq process, loads all dependencies from `.vo` files, processes every sentence from scratch, and exits. If you change one tactic in the middle of a file, the entire prefix must be re-executed. If you add a `Check` or `Print` query to inspect intermediate state, the file must be re-compiled from scratch.

The interactive IDE story (VsRocq) solves this for editor use — it maintains a persistent Rocq process, tracks sentence boundaries, and navigates forward/back as the user edits. But this machinery is locked behind an LSP server coupled to VS Code. There is no way to get these benefits from the command line, from a Makefile, from CI, or from any non-VS-Code editor without speaking LSP.

`rocqd` is a daemon that brings incremental, cached Rocq compilation to the command line. The interface is simply `rocq compile`, but under the hood, persistent Rocq instances are maintained, state is re-used by navigating forward and back in response to source changes, and queries can be injected without losing any compilation state.

## Background: How VsRocq Works

VsRocq (the official Rocq language server at `rocq-prover/vsrocq`) is the primary reference implementation for interactive Rocq document management. Understanding its architecture is essential for daemon design.

### Architecture

VsRocq compiles into a single binary `vsrocqtop` that links directly against the Rocq OCaml API. It speaks LSP over stdin/stdout via JSON-RPC. The architecture has seven components arranged in layers:

**Vscoqtop** → **LSPManager** → **DocumentManager** → **Document** / **Scheduler** / **Queries**; **LSPManager** → **ExecutionManager** → **DelegationManager**.

The **Document** module represents a file as a sequence of sentences — commands terminated by `. ` (period + whitespace). It uses Rocq's own parser to determine sentence boundaries, which is necessary because Rocq's extensible notation system makes naive splitting impossible. Each sentence gets a unique ID, and a `sentences_by_end` map provides efficient position-based lookup.

The **Scheduler** performs static dependency analysis between sentences to determine what can parallelize and what the minimal re-execution set is after an edit.

The **ExecutionManager** maintains a mapping from sentence IDs to Rocq execution states. It calls into Rocq's Vernac interpretation API (`Vernac.process_expr`) for execution. The Rocq `Vernac.State.t` type carries the full state needed to continue processing.

The **Queries** module handles `Check`, `Search`, `About`, `Locate`, and `Print` commands by calling the Rocq API directly for read-only access against an already-computed state, separate from the Vernac interpretation pipeline. This architectural separation is the key insight: queries read existing states; the ExecutionManager writes them.

### Proof Checking Modes

VsRocq supports two modes. **Manual mode** (default) requires explicit step-by-step navigation — the user sends `StepForward`, `StepBackward`, or `InterpretToPoint` commands. **Continuous mode** automatically checks the document as the user types.

### Custom LSP Extensions

Beyond standard LSP, VsRocq defines custom notifications: `InterpretToPoint`, `InterpretToEnd`, `StepForward`, `StepBackward` (client → server) and `ProofView`, `MoveCursor`, `BlockOnError` (server → client). Custom requests include `About`, `Check`, `Locate`, `Print`, `Search`, `Reset`, `DocumentState`, and `DocumentProofs`.

### Delegation

VsRocq supports three delegation strategies: **None** (sequential), **Skip** (skip out-of-focus proofs), and **Delegate** (worker processes check proofs in parallel). In Delegate mode, the DelegationManager spawns separate OS processes. When a `Proof...Qed` block is encountered, the Rocq state is forked to a worker, the master continues past Qed with an opaque placeholder, and the result is substituted when the worker finishes. Memory scales linearly with worker count. Proofs below a configurable threshold (default 0.03s) are not delegated.

### Incremental Document Handling

When a `didChange` notification arrives, VsRocq applies the edit to raw text, computes a diff between old and new parsed documents, shifts unchanged sentences after the edit point, re-parses only affected regions, and invalidates Rocq states for changed sentences and all dependents. A v2.2.0 optimization made each line an independent parse event, allowing cancellation and re-parsing only the latest version.

### Ongoing Upstream Work: SOCCA

The VsRocq maintainers (primarily @gares) are actively refactoring the language server under a milestone called **SOCCA** with labels like "ADT: generic lang-serv". Issues #1096–#1100 (May 2025) aim to extract the interaction/navigation code out of the LSP-specific "Bridge" layer, making the document manager and execution machinery reusable as a generic library. This is directly aligned with our daemon goals, and we should track and potentially contribute to this effort.

## The Rocq OCaml API

The daemon will link against the same Rocq OCaml API that VsRocq uses. The key interfaces are:

### `Vernac.process_expr : state:Vernac.State.t -> Vernacexpr.vernac_control -> Vernac.State.t`

This is the central function. It takes a Rocq state and a parsed vernacular command and returns a new state. The state is a value (in the OCaml sense) — it can be saved and restored.

### `Vernac.State.t`

In Rocq 8.16, this was a record with fields `doc : Stm.doc`, `sid : Stateid.t`, `proof : Proof.t option`, and `time : bool`. In Rocq 9.0+, the structure has evolved but the basic idea remains: a `Vernac.State.t` captures everything needed to continue processing from that point. The STM (State Transaction Machine) still exists but VsRocq's ExecutionManager wraps it at a higher level.

### `Vernac.load_vernac`

Loads an entire file on top of a given state. Useful for dependency loading.

### The Summary and Freeze/Unfreeze Mechanism

Rocq's global state is managed through a "summary" system. Each module (kernel, library, tactics, notations, etc.) registers its mutable state with `Summary.declare_summary`, providing freeze/unfreeze functions. A `Summary.frozen` value is a snapshot of all registered tables. This is how backtracking works internally: freeze the state before executing a sentence, and unfreeze to revert.

### Parsing

Rocq's parser is extensible at runtime via notations and plugins. This means parsing depends on execution state — a sentence's parse can only be determined after all preceding sentences have been executed (because they may extend the grammar). VsRocq handles this by using `Pcoq.parse_vernac` against the current state at each sentence boundary.

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
│  │  Document     │ │  Document          │     │
│  │  Scheduler    │ │  Scheduler         │     │
│  │  StateCache   │ │  StateCache        │     │
│  │  ExecManager  │ │  ExecManager       │     │
│  └───────────────┘ └────────────────────┘     │
│                                               │
│  ┌─────────────────────────────────────────┐  │
│  │        Rocq Instance Pool               │  │
│  │  (linked Rocq OCaml API)                │  │
│  └─────────────────────────────────────────┘  │
│                                               │
│  ┌─────────────────────────────────────────┐  │
│  │        Disk Cache                        │  │
│  │  ~/.cache/rocqd/                         │  │
│  └─────────────────────────────────────────┘  │
└───────────────────────────────────────────────┘
```

### Component: File Session

A File Session manages one `.v` file. It is the daemon-side analog of VsRocq's DocumentManager, but decoupled from LSP. A session maintains:

**Parsed document.** A sequence of `(sentence_id, source_text, byte_range)` triples. Parsing is performed using Rocq's own parser against the execution state at each sentence boundary.

**State cache.** A map from `sentence_id` to `Vernac.State.t`. This is the core caching mechanism. When the source changes, the daemon computes the longest unchanged prefix (by comparing sentence text), and the cached states for unchanged sentences remain valid. Only the changed suffix needs re-execution.

**Execution cursor.** The index of the last fully-executed sentence. Everything up to and including this sentence has a cached state.

**File content hash.** Used for quick staleness checks and disk cache keys.

#### Session Lifecycle

1. **Open**: Client requests compilation of `foo.v`. The daemon reads the file, parses it sentence by sentence (each parse depends on the state after the previous sentence), and begins execution.

2. **Execute**: Sentences are executed sequentially via `Vernac.process_expr`. After each sentence, the resulting state is stored in the state cache. Execution continues until the end of the file or an error.

3. **Re-use on change**: When `foo.v` is compiled again (e.g., after an edit), the daemon re-reads the file, re-parses, and diffs against the cached document. The longest common prefix of sentences retains its cached states. Re-execution starts from the first changed sentence.

4. **Evict**: Sessions are evicted under memory pressure (configurable limit, default 4 GB — matching VsRocq's default). LRU eviction. Eviction discards in-memory states but may preserve a disk-cache checkpoint (see Disk Cache below).

#### Diff Algorithm

The diff is sentence-level, not character-level. Two sentences are "the same" if their source text is identical (after whitespace normalization). The algorithm:

1. Parse the new file content into sentences using Rocq's parser against the cached execution states (up to the point where states are available; beyond that, parse incrementally).
2. Walk the old and new sentence lists. While sentences match, keep the cached state.
3. At the first mismatch, invalidate all cached states from that point forward.
4. Special case: if the only change is *appending* sentences, no invalidation occurs — just execute the new suffix.
5. Special case: insertions/deletions in the middle invalidate from the change point. In the future, the Scheduler (see below) could reduce this to only invalidate dependents, but the initial implementation uses conservative suffix invalidation.

Note the subtlety with parsing: because Rocq's parser is stateful (notations, etc.), a change to sentence N may change how sentence N+1 parses. The daemon must re-parse from the first changed sentence, using the cached state at that point. If sentence N changes and its execution produces a different parser state (e.g., a `Notation` command was modified), all subsequent sentences must be re-parsed. If the parser state is unchanged (e.g., only a tactic in a proof body changed), subsequent sentences' parses are still valid.

### Component: Query Injection

This is the key innovation for the daemon use case. Queries (`Check`, `Print`, `About`, `Search`, `Locate`, `Compute`) are **side-effect-free** in Rocq's semantics. They produce output but do not modify the proof state, the global environment, or the parser state.

The daemon exploits this property:

**Query extraction.** Before diffing, the daemon scans the source for query commands. These are identified by Rocq's command classifier (VsRocq uses `CLASSIFIED AS QUERY` in the `VERNAC COMMAND EXTEND` DSL; we use `Vernacprop.under_control` to check if a command has side effects).

**Transparent query execution.** When a query appears in the source, the daemon:
1. Finds the nearest preceding non-query sentence with a cached state.
2. Executes the query against that state (via `Vernac.process_expr` — queries still go through Vernac, they just don't modify the state that matters for subsequent sentences).
3. Captures the output (via Rocq's feedback mechanism).
4. Does **not** store the post-query state as the state for subsequent sentences. The next sentence still executes against the pre-query state.

This means: adding `Check nat.` in the middle of a file does not invalidate any cached states after it. The daemon simply executes the query against the existing cached state and returns the result.

**Batched queries.** Multiple queries at the same point share a single state lookup. Queries interspersed between regular sentences each execute against the state produced by the preceding regular sentence.

**Query-only mode.** A dedicated `rocqd query foo.v:42 "Check nat."` command executes a query against the state at line 42 of an already-compiled file, without modifying the file or losing any state. This is the programmatic analog of VsRocq's query panel.

### Component: State Cache

The state cache operates at two levels:

**In-memory cache.** Maps `(file_path, sentence_index)` → `Vernac.State.t`. This is the hot cache. States are full OCaml values held in the daemon's heap. Backtracking is instantaneous — just index into the cache.

**Disk cache** (stretch goal). The in-memory cache is lost when the daemon restarts. For large projects, re-building state from scratch on daemon restart is expensive. The disk cache serializes select states to `~/.cache/rocqd/`.

The disk cache is challenging because `Vernac.State.t` contains closures, mutable references to the global environment, and other values that are not trivially serializable. Two approaches:

*Option A: Checkpoint via `.vo` replay.* Instead of serializing raw Rocq state, the disk cache stores the `.vo` file that `Require` would load. On daemon restart, the daemon can `Require` the cached module to restore state up to a file boundary, then incrementally execute from there. This piggybacks on Rocq's existing serialization. The limitation is that it only works at file granularity — you can cache "the state after compiling `foo.v`" but not "the state at line 42 of `foo.v`".

*Option B: Replay log.* Store the sentence list and re-execute from the last available state (typically the post-`Require` state). This is slower than true state serialization but requires no new serialization infrastructure.

*Option C: Marshal + Summary.* The Summary system's freeze/unfreeze is designed for snapshotting. Investigate whether `Summary.frozen` values are marshallable. The `async_proofs_cache force` flag already forces the STM to cache more states internally — this machinery might be reusable.

### Component: Request Router

The daemon listens on a Unix domain socket (default `$XDG_RUNTIME_DIR/rocqd.sock`) and handles JSON-RPC requests. The protocol is intentionally minimal:

```
// Compile a file. Returns diagnostics (errors, warnings).
// The daemon caches state for future re-use.
compile(file: string, flags: string[]) → {
  diagnostics: Diagnostic[],
  compiled_vo: string | null  // path to .vo if successful
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
  sessions: { file: string, sentences: int, cursor: int, memory_mb: int }[],
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
rocq compile foo.v          # daemon compiles, caches state
# ... user edits foo.v ...
rocq compile foo.v          # daemon re-uses prefix, re-executes only changes
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

**On first compile.** The `Require` sentences are executed normally, loading `.vo` files. The resulting states are cached.

**When a dependency changes.** If `bar.vo` is recompiled, all files that `Require` it must be invalidated. The daemon can detect this by:
- Watching `.vo` file mtimes.
- Tracking which `Require` commands each session has executed and what `.vo` files they resolved to.
- On the next `compile` request, checking if any `Require`'d `.vo` has changed since the cached state was computed.

If a dependency changed, the session is invalidated from the `Require` sentence that loaded it. In the common case where `Require` commands are at the top of the file, this means re-executing the entire file — but the `Require` itself is fast (it loads a `.vo`, not re-compiles from source). The expensive work is only re-done for sentences that depend on the changed definitions.

### Multi-Instance Support (Future)

Initially, the daemon runs a single Rocq instance (single OCaml process, single global state). This is the VsRocq model — one language server process per window.

For parallel compilation of multiple files, the daemon can manage a pool of worker processes, each running a Rocq instance. The pool manager would:

1. Parse dependency graphs from `_CoqProject` / `rocq dep`.
2. Assign files to workers respecting dependency order.
3. Route `compile` requests to the worker that has the most relevant cached state.
4. Support work-stealing: if worker A is idle and worker B has a queued request for a file with no cached state anywhere, A can take it.

This parallels VsRocq's DelegationManager but at file granularity rather than proof granularity. The two levels compose: each worker could internally delegate proof checking to sub-workers.

### Memory Management

Rocq processes are memory-hungry. A single file compilation can consume hundreds of MB or several GB for large developments. The daemon must be aggressive about memory:

**Per-session memory tracking.** Estimate memory per session via `Gc.stat` deltas or `Obj.reachable_words` sampling.

**LRU eviction.** When total memory exceeds the limit (configurable, default 4 GB), evict the least-recently-used session. Eviction discards all cached states for that file.

**Selective state dropping.** Not all sentence states need to be cached. In a long proof, intermediate tactic states are expensive to keep. A heuristic: cache states at "section boundaries" — after `Qed`, after `Require`, after `Definition`/`Fixpoint`/`Inductive`, at the beginning of each `Section`/`Module`. Drop intermediate proof states. When backtracking into a proof, re-execute from the nearest cached sentence.

**GC coordination.** After evicting sessions, call `Gc.compact` to return memory to the OS.

## Implementation Plan

### Phase 1: Single-file daemon with in-memory caching

- OCaml binary that links against Rocq's API.
- Unix socket listener, JSON-RPC protocol.
- Single file session: parse, execute, cache states.
- Re-use on recompile: sentence-level diff, prefix reuse.
- Query injection: transparent handling of `Check`/`Print`/etc.
- CLI wrapper script.

### Phase 2: Dependency tracking and multi-file support

- Track `Require` dependencies and `.vo` mtimes.
- Invalidate sessions when dependencies change.
- Parse `_CoqProject` for load paths.
- Support multiple concurrent file sessions.

### Phase 3: Disk cache and persistence

- Implement Option A (`.vo` replay) for cross-restart caching.
- Investigate state serialization for finer-grained disk caching.

### Phase 4: Multi-instance pool

- Worker process pool for parallel file compilation.
- Dependency-aware scheduling.
- Integration with `rocq_makefile` / `dune` build systems.

### Phase 5: Upstream integration

- Coordinate with SOCCA refactoring to share code with VsRocq.
- Propose `rocq compile --daemon` flag upstream.
- Potentially merge the daemon's document manager with VsRocq's generic language server library.

## Key Risks and Open Questions

**State validity.** Rocq's global state is complex (Summary tables, kernel state, notation state, tactic state, etc.). We rely on the assumption that `Vernac.State.t` fully captures the execution state — that restoring it and continuing execution is equivalent to replaying from scratch. VsRocq makes this assumption and it works, but there may be edge cases with plugins, side-effecting commands (`Redirect`, `Extraction`), or native compilation.

**Parser statefulness.** Because parsing depends on execution state (notations, `Declare Custom Entry`, etc.), a change to an early sentence can change how all subsequent sentences parse. The conservative approach (re-parse from the change point) is correct but may be slow for changes near the top of large files. A potential optimization: hash the parser state (`Pcoq` state) after each sentence and skip re-parsing of subsequent sentences if the parser state hash is unchanged.

**Memory overhead of state caching.** Storing `Vernac.State.t` for every sentence may be prohibitively expensive. Sharing is key — most of the state (global environment, libraries) is shared between consecutive sentences via OCaml's heap. But long proofs generate large proof states that are not shared. Selective caching (Phase 1) and state compression (future) are mitigations.

**Coexistence with VsRocq.** If a user has both VsRocq (in their editor) and `rocqd` running, they manage independent Rocq instances. There is no state sharing. This is fine but wasteful. Long-term, VsRocq and `rocqd` could share a single daemon process — this aligns with the SOCCA vision of separating the language server core from the LSP transport.

**Race conditions.** File changes during compilation, concurrent compile requests for the same file, etc. The daemon should serialize requests per file (one execution at a time per session) and use file content hashing (not mtimes) for staleness detection.

## Language Choice

OCaml. The daemon must link against Rocq's OCaml API. There is no practical alternative — the `Vernac.process_expr` function, the state types, the parser, the Summary system — all are OCaml. The daemon is an OCaml program that happens to also run a socket server.

For the CLI wrapper, a simple shell script or a small Go/Rust binary that connects to the Unix socket would work. The wrapper need not be OCaml.

## Relation to Other Projects

**VsRocq.** The most closely related project. `rocqd` is essentially "VsRocq's document manager, but with a Unix socket instead of LSP, and aimed at batch compilation instead of interactive editing." We should share as much code as possible, especially after SOCCA lands.

**coq-lsp / rocq-lsp.** An alternative language server with a different architecture (Flèche engine, Pétanque API). Has useful ideas (memo-based caching, `state/hash`) but its maintenance situation is uncertain. We track it for ideas but don't depend on it.

**SerAPI.** Deprecated predecessor to coq-lsp. Introduced the idea of separating `Add` (parse) from `Exec` (execute) with explicit state IDs. The concepts are subsumed by the Vernac API approach.

**The STM (State Transaction Machine).** Still exists in Rocq 9.x. The STM maintains a DAG of states with `add`/`observe`/`edit_at` operations. VsRocq wraps it via the ExecutionManager. The daemon could use the STM directly or bypass it and use `Vernac.process_expr` directly (as VsRocq effectively does). Using `Vernac.process_expr` is simpler and sufficient.

## Appendix: VsRocq Custom Protocol Reference

For reference, the custom LSP messages defined by VsRocq in `extProtocol.ml`:

**Client → Server Notifications:** `vsrocq/interpretToPoint { textDocument, position }`, `vsrocq/interpretToEnd { textDocument }`, `vsrocq/stepForward { textDocument }`, `vsrocq/stepBackward { textDocument }`.

**Server → Client Notifications:** `vsrocq/proofView { proof, messages }`, `vsrocq/moveCursor { uri, range }`, `vsrocq/blockOnError { uri, range, message }`, `vsrocq/coqLogMessage { message }`.

**Custom Requests (Client → Server):** `vsrocq/reset`, `vsrocq/about { textDocument, position, pattern }`, `vsrocq/check { textDocument, position, pattern }`, `vsrocq/locate { textDocument, position, pattern }`, `vsrocq/print { textDocument, position, pattern }`, `vsrocq/search { textDocument, position, pattern }`, `vsrocq/documentState { textDocument }`, `vsrocq/documentProofs { textDocument }`.

**Settings:** `proof.mode: Continuous | Manual`, `proof.delegation: None | Skip | Delegate`, `proof.workers: int`, `proof.block: bool`, `proof.pointInterpretation: Cursor | NextCommand`.

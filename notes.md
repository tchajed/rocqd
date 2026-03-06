# Research Notes: VsRocq, Rocq Internals, and Related Projects

## How VsRocq Works

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

The key interfaces for driving Rocq programmatically:

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

## VsRocq Capability Investigation

Investigation conducted March 2026 against the `rocq-prover/vsrocq` repository.

### .vo File Generation: Not Supported

VsRocq cannot emit `.vo` files. There is no call to `Library.save` or any Rocq kernel serialization API anywhere in the codebase. The `ExecutionManager` and `DocumentManager` are exclusively focused on in-memory interactive proof checking.

Issue #277 ("Shortcut for CoqIDE Compile => Compile Buffer") confirms this is a known gap — the maintainer's response was "I personally just hit `make` or `dune build` in the integrated terminal." Issue #252 ("Feature: compile before require") requests automatic dependency compilation and remains open. Issue #768 ("Handling external compilation") discusses reacting to externally-compiled `.vo` files, not generating them.

**Implication for rocqd:** If using the Rust-over-LSP architecture, `.vo` generation requires a separate `rocq compile` pass. The daemon provides fast incremental *checking*; artifact generation falls back to the standard compiler.

### Sentence Boundary Exposure: Partially Available

VsRocq internally maintains a full list of parsed sentences with exact byte offsets (the `sentence` type in `document.ml` has `parsing_start`, `start`, `stop` fields). However, no single protocol endpoint exposes the complete sentence list as structured data.

What IS available:

- **`prover/moveCursor` notification** — after each `stepForward`/`stepBackward`, sends `{ uri, range }` with the `Range` of the sentence just executed/un-executed. Gives boundaries one sentence at a time.
- **`prover/updateHighlights` notification** — sends `{ preparedRange, processingRange, processedRange }`, each a list of `Range`. These are **aggregated** — adjacent checked sentences are merged into contiguous regions, so individual sentence boundaries are lost.
- **`prover/documentProofs` request** — returns structured `proof_block` objects with ranges for theorem statements and individual tactic steps. Only covers `TheoremKind` proof blocks, not definitions/sections/modules/other vernacular.
- **`prover/documentState` request** — returns a human-readable debug string listing all sentences with execution status. Not structured data; not useful for programmatic consumption.

What is NOT available:

- A complete, structured list of all sentence boundaries (the internal `Document.sentences_sorted_by_loc` data).
- Sentence-level execution status for non-proof sentences in structured form.

**Implication for rocqd:** For the Rust-over-LSP architecture, this gap is acceptable because VsRocq handles sentence-level diffing internally. The daemon sends `didChange` with file content, and VsRocq determines sentences. For query injection, the daemon passes a *position* to `prover/check` etc., and VsRocq resolves it to the correct state. The daemon does not need to know sentence boundaries itself. If finer control is ever needed, contributing a `prover/documentSentences` endpoint upstream would be straightforward — the data already exists internally.

## Related Projects

**coq-lsp / rocq-lsp.** An alternative language server with a different architecture (Flèche engine, Pétanque API). Has useful ideas (memo-based caching, `state/hash`) but its maintenance situation is uncertain. We track it for ideas but don't depend on it.

**SerAPI.** Deprecated predecessor to coq-lsp. Introduced the idea of separating `Add` (parse) from `Exec` (execute) with explicit state IDs. The concepts are subsumed by the Vernac API approach.

**The STM (State Transaction Machine).** Still exists in Rocq 9.x. The STM maintains a DAG of states with `add`/`observe`/`edit_at` operations. VsRocq wraps it via the ExecutionManager. The daemon could use the STM directly or bypass it and use `Vernac.process_expr` directly (as VsRocq effectively does). Using `Vernac.process_expr` is simpler and sufficient.

## Appendix: VsRocq Custom Protocol Reference

For reference, the custom LSP messages defined by VsRocq in `extProtocol.ml`:

**Client → Server Notifications:** `vsrocq/interpretToPoint { textDocument, position }`, `vsrocq/interpretToEnd { textDocument }`, `vsrocq/stepForward { textDocument }`, `vsrocq/stepBackward { textDocument }`.

**Server → Client Notifications:** `vsrocq/proofView { proof, messages }`, `vsrocq/moveCursor { uri, range }`, `vsrocq/blockOnError { uri, range, message }`, `vsrocq/coqLogMessage { message }`.

**Custom Requests (Client → Server):** `vsrocq/reset`, `vsrocq/about { textDocument, position, pattern }`, `vsrocq/check { textDocument, position, pattern }`, `vsrocq/locate { textDocument, position, pattern }`, `vsrocq/print { textDocument, position, pattern }`, `vsrocq/search { textDocument, position, pattern }`, `vsrocq/documentState { textDocument }`, `vsrocq/documentProofs { textDocument }`.

**Settings:** `proof.mode: Continuous | Manual`, `proof.delegation: None | Skip | Delegate`, `proof.workers: int`, `proof.block: bool`, `proof.pointInterpretation: Cursor | NextCommand`.

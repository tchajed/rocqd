# VsRocq LSP Protocol Reference

Practical reference for implementing an LSP client that talks to `vsrocqtop`, the
Rocq language server from the [rocq-prover/vsrocq](https://github.com/rocq-prover/vsrocq)
repository. Derived from the OCaml source in `language-server/protocol/` and the
TypeScript client in `client/src/`.

All custom method names use the **`prover/`** prefix (not `vsrocq/`).

---

## Transport and Framing

`vsrocqtop` communicates over **stdin/stdout** using the standard LSP wire format:

```
Content-Length: <byte-count>\r\n
\r\n
<JSON-RPC 2.0 payload>
```

Every message is a JSON-RPC 2.0 envelope. Requests have `id`, `method`, `params`.
Notifications have `method`, `params` (no `id`). Responses have `id`, `result` or
`error`.

The server ignores SIGINT. It reads from stdin and writes to stdout; stderr may
carry debug output depending on build flags.

---

## Standard LSP Methods

### initialize (request)

The client sends the standard `initialize` request. VsRocq extracts its custom
settings from `initializationOptions` (see [Initialization Options](#initialization-options)
below). The server declares these capabilities:

| Capability | Value |
|---|---|
| `textDocumentSync` | `Incremental` (kind 2) |
| `completionProvider` | Enabled, `resolveProvider: false` |
| `hoverProvider` | `true` |
| `definitionProvider` | `true` |
| `documentSymbolProvider` | Enabled |

After receiving the `initialize` response, the client must send `initialized` as
usual.

### textDocument/didOpen (notification)

Standard LSP. Opens a document for management. The server begins parsing
immediately.

### textDocument/didChange (notification)

Standard LSP with **incremental** sync. Send `TextDocumentContentChangeEvent`
objects with `range` and `text`. The server applies the edit to its internal
document, re-parses affected regions, and invalidates execution states for
changed sentences and their dependents.

### textDocument/didClose (notification)

Standard LSP. Releases the document.

### textDocument/didSave (notification)

Standard LSP. No special behavior.

### textDocument/completion (request)

Standard LSP completion. Only active when `completion.enable` is `true` in
settings. Supports two ranking algorithms (see settings).

### textDocument/hover (request)

Standard LSP hover.

### textDocument/definition (request)

Standard LSP go-to-definition.

### textDocument/documentSymbol (request)

Standard LSP document symbols.

### textDocument/publishDiagnostics (notification, server -> client)

Standard LSP. The server pushes diagnostics as sentences are checked. Controlled
by the `diagnostics.enable` setting.

### shutdown / exit

Standard LSP shutdown sequence.

---

## Initialization Options

Settings are passed in the `initializationOptions` field of `InitializeParams`.
The JSON object has this structure (OCaml source: `settings.ml`):

```json
{
  "proof": {
    "mode": 0,
    "delegation": "None",
    "workers": 1,
    "block": true,
    "pointInterpretationMode": 0
  },
  "goals": {
    "diff": { "mode": "Off" },
    "messages": { "full": true }
  },
  "completion": {
    "enable": false,
    "algorithm": 1,
    "unificationLimit": 100,
    "atomicFactor": 5.0,
    "sizeFactor": 5.0
  },
  "diagnostics": {
    "enable": true,
    "full": false
  },
  "memory": {
    "limit": 4
  }
}
```

### proof.mode

| Value | Meaning |
|---|---|
| `0` (Manual) | Checking only advances on explicit commands (`stepForward`, `interpretToPoint`, etc.) |
| `1` (Continuous) | The server automatically checks the document as it changes |

Default: `0` (Manual).

### proof.delegation

| Value | Meaning |
|---|---|
| `"None"` | Sequential execution, no parallelism |
| `"Skip"` | Skip proofs that are out of focus |
| `"Delegate"` | Spawn worker processes for parallel proof checking |

Default: `"None"`.

### proof.workers

Integer. Number of worker processes when delegation is `"Delegate"`. Default: `1`.

### proof.block

Boolean. When `true`, execution stops at the first error. When `false`, the
server attempts to continue past errors. Default: `true`.

### proof.pointInterpretationMode

| Value | Meaning |
|---|---|
| `0` (Cursor) | `interpretToPoint` checks up to the cursor position |
| `1` (NextCommand) | `interpretToPoint` checks up to the next command after the cursor |

Default: `0` (Cursor).

### goals.diff.mode

| Value | Meaning |
|---|---|
| `"Off"` | No diff display |
| `"On"` | Show diffs between goal states |
| `"Removed"` | Show removed hypotheses |

### goals.messages.full

Boolean. When `true`, show full messages in the goal panel. Default: `true`.

### completion.enable

Boolean. Enable/disable code completion. Default: `false`.

### completion.algorithm

| Value | Meaning |
|---|---|
| `0` | SplitTypeIntersection |
| `1` | StructuredSplitUnification |

Default: `1`.

### diagnostics.enable

Boolean. Enable/disable publishing diagnostics. Default: `true`.

### diagnostics.full

Boolean. When `true`, show full diagnostic details. Default: `false`.

### memory.limit

Integer (GiB). Memory limit above which the server discards execution states.
Default: `4`.

---

## Custom Notifications: Client to Server

### prover/interpretToPoint

Requests the server to check the document up to a specific position.

**Method:** `"prover/interpretToPoint"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier,  // { uri, version }
  position: Position                              // { line, character }
}
```

OCaml type: `InterpretToPointParams.t`
- `textDocument : VersionedTextDocumentIdentifier.t`
- `position : Position.t`

### prover/interpretToEnd

Requests the server to check the entire document.

**Method:** `"prover/interpretToEnd"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier   // { uri, version }
}
```

### prover/stepForward

Requests the server to execute the next sentence.

**Method:** `"prover/stepForward"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier   // { uri, version }
}
```

### prover/stepBackward

Requests the server to retract the last executed sentence.

**Method:** `"prover/stepBackward"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier   // { uri, version }
}
```

---

## Custom Notifications: Server to Client

### prover/updateHighlights

Sent whenever the execution state changes. Reports which ranges of the document
are in each processing state.

**Method:** `"prover/updateHighlights"`

**Params:**
```typescript
{
  uri: DocumentUri,                // e.g. "file:///path/to/file.v"
  preparedRange: Range[],          // parsed but not yet executing
  processingRange: Range[],        // currently being checked
  processedRange: Range[]          // successfully checked
}
```

OCaml type: `overview`
- `uri : DocumentUri.t`
- `preparedRange : Range.t list`
- `processingRange : Range.t list`
- `processedRange : Range.t list`

Note: adjacent checked sentences are merged into contiguous ranges, so individual
sentence boundaries are not preserved.

### prover/proofView

Sent when the proof state changes (after stepping into a proof, executing a
tactic, etc.). Contains both a structured `pp` representation and a
pre-rendered string representation.

**Method:** `"prover/proofView"`

**Params:**
```typescript
{
  range: Range,
  proof: ProofState | null,
  messages: [DiagnosticSeverity, Pp][],
  pp_proof: PpProofState | null,
  pp_messages: [DiagnosticSeverity, string][]
}
```

Where `ProofState` (OCaml: `ProofState.t`) is:
```typescript
{
  goals: Goal[],
  shelvedGoals: Goal[],
  givenUpGoals: Goal[],
  unfocusedGoals: Goal[]
}
```

And `Goal` (OCaml: `ProofState.goal`) is:
```typescript
{
  id: number,
  hypotheses: Pp[],
  goal: Pp
}
```

The `PpProofState` (OCaml: `PpProofState.t`) uses plain strings instead of the
`Pp` AST:
```typescript
{
  goals: PpGoal[],
  shelvedGoals: PpGoal[],
  givenUpGoals: PpGoal[],
  unfocusedGoals: PpGoal[]
}
```

Where `PpGoal` (OCaml: `PpProofState.goal`) is:
```typescript
{
  id: number,
  hypotheses: PpHypothesis[],
  goal: string
}
```

And `PpHypothesis` (OCaml: `PpProofState.hypothesis`) is:
```typescript
{
  ids: string[],
  body: string | null,
  _type: string
}
```

### prover/moveCursor

Sent after `stepForward`/`stepBackward` to indicate where the cursor should move.

**Method:** `"prover/moveCursor"`

**Params:**
```typescript
{
  uri: DocumentUri,
  range: Range
}
```

### prover/blockOnError

Sent when execution encounters an error and `proof.block` is `true`.

**Method:** `"prover/blockOnError"`

**Params:**
```typescript
{
  uri: DocumentUri,
  range: Range
}
```

### prover/searchResult

Sent as results stream in for a `prover/search` request. Each notification
carries one result.

**Method:** `"prover/searchResult"`

**Params:**
```typescript
{
  id: string,
  name: Pp,
  statement: Pp
}
```

OCaml type: `query_result`
- `id : string`
- `name : pp`
- `statement : pp`

### prover/debugMessage

Log/debug messages from the server.

**Method:** `"prover/debugMessage"`

**Params:**
```typescript
{
  message: string
}
```

Note: the OCaml type is named `RocqLogMessageParams`, but the wire method name
is `"prover/debugMessage"`.

---

## Custom Requests: Client to Server

### prover/resetRocq

Resets the document's execution state.

**Method:** `"prover/resetRocq"`

**Params:**
```typescript
{
  textDocument: TextDocumentIdentifier   // { uri }
}
```

**Response:** `null` (unit)

### prover/about

Queries information about a term (similar to Rocq's `About` command).

**Method:** `"prover/about"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier,
  position: Position,
  pattern: string
}
```

**Response:** `Pp` (the result formatted as a Pp AST)

### prover/check

Type-checks an expression (similar to Rocq's `Check` command).

**Method:** `"prover/check"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier,
  position: Position,
  pattern: string
}
```

**Response:** `Pp`

### prover/locate

Locates a definition (similar to Rocq's `Locate` command).

**Method:** `"prover/locate"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier,
  position: Position,
  pattern: string
}
```

**Response:** `Pp`

### prover/print

Prints a term's definition (similar to Rocq's `Print` command).

**Method:** `"prover/print"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier,
  position: Position,
  pattern: string
}
```

**Response:** `Pp`

### prover/search

Initiates a search query. Results arrive asynchronously via `prover/searchResult`
notifications. The `id` field correlates results back to the request.

**Method:** `"prover/search"`

**Params:**
```typescript
{
  textDocument: VersionedTextDocumentIdentifier,
  position: Position,
  pattern: string,
  id: string
}
```

**Response:** `null` (unit; results come via notifications)

### prover/documentState

Returns a human-readable debug dump of the document's internal state.

**Method:** `"prover/documentState"`

**Params:**
```typescript
{
  textDocument: TextDocumentIdentifier   // { uri }
}
```

**Response:**
```typescript
{
  document: string
}
```

Note: the returned string is for debugging; it is not structured data.

### prover/documentProofs

Returns structured information about proof blocks in the document.

**Method:** `"prover/documentProofs"`

**Params:**
```typescript
{
  textDocument: TextDocumentIdentifier   // { uri }
}
```

**Response:**
```typescript
{
  proofs: ProofBlock[]
}
```

Where `ProofBlock` is:
```typescript
{
  statement: ProofStatement,
  range: Range,
  steps: ProofStep[]
}
```

`ProofStatement`:
```typescript
{
  statement: string,
  range: Range
}
```

`ProofStep`:
```typescript
{
  tactic: string,
  range: Range
}
```

Only covers `Theorem`-kind proof blocks; definitions, sections, modules, and
other vernacular are not included.

---

## The Pp Type

Many responses use a `Pp` (pretty-print) AST instead of flat strings. This is
Rocq's internal formatting representation, serialized as JSON. The OCaml
definition (from `printing.ml`):

```ocaml
type pp_tag = string

type block_type =
  | Pp_hbox
  | Pp_vbox   of int
  | Pp_hvbox  of int
  | Pp_hovbox of int

type pp =
  | Ppcmd_empty
  | Ppcmd_string of string
  | Ppcmd_glue of pp list
  | Ppcmd_box of block_type * pp
  | Ppcmd_tag of pp_tag * pp
  | Ppcmd_print_break of int * int
  | Ppcmd_force_newline
  | Ppcmd_comment of string list
```

Because these use `[@@deriving yojson]`, the JSON serialization follows the
`ppx_yojson_conv` convention for OCaml variants:

- Nullary constructors: `["Ppcmd_empty"]` (a single-element JSON array)
- Unary constructors: `["Ppcmd_string", "hello"]` (two-element array)
- Multi-argument constructors: `["Ppcmd_box", ["Pp_hbox"], ["Ppcmd_string", "x"]]`
- Polymorphic variant-style: constructors become JSON arrays where the first
  element is the constructor name as a string

For example, the Rocq expression `nat -> nat` might serialize as:
```json
["Ppcmd_glue", [
  ["Ppcmd_string", "nat"],
  ["Ppcmd_string", " ->"],
  ["Ppcmd_print_break", 1, 0],
  ["Ppcmd_string", "nat"]
]]
```

To render `Pp` to a plain string: recursively walk the tree, emit string
contents, translate `Ppcmd_print_break(nspaces, _)` to spaces (or newlines
depending on box context), and `Ppcmd_force_newline` to `\n`. For a simple
flat rendering, ignore boxes and tags.

---

## Typical Session Sequence

A minimal session in Manual mode:

```
Client                              Server
  |                                   |
  |-- initialize ------------------>  |
  |<-- initialize result -----------  |   (capabilities, including textDocumentSync: Incremental)
  |-- initialized ----------------->  |
  |                                   |
  |-- textDocument/didOpen -------->  |
  |<-- prover/updateHighlights ----  |   (empty ranges initially)
  |                                   |
  |-- prover/stepForward ---------->  |
  |<-- prover/updateHighlights ----  |   (processedRange grows)
  |<-- prover/moveCursor ----------  |   (cursor moves past executed sentence)
  |<-- prover/proofView ----------  |   (if inside a proof)
  |                                   |
  |-- prover/interpretToPoint ----->  |
  |<-- prover/updateHighlights ----  |   (multiple updates as sentences execute)
  |<-- prover/moveCursor ----------  |   (final cursor position)
  |<-- prover/proofView ----------  |
  |                                   |
  |-- prover/check --------------->  |   (query at a position)
  |<-- response (Pp) --------------  |
  |                                   |
  |-- textDocument/didChange ------>  |
  |<-- prover/updateHighlights ----  |   (invalidated ranges disappear)
  |<-- textDocument/publishDiagnostics  |
  |                                   |
  |-- shutdown -------------------->  |
  |<-- shutdown result -------------  |
  |-- exit ------------------------>  |
```

In Continuous mode, the `stepForward`/`interpretToPoint` commands are
unnecessary; the server automatically checks the document after `didOpen` and
`didChange`.

---

## Summary of All Custom Method Names

| Method | Direction | Kind |
|---|---|---|
| `prover/interpretToPoint` | client -> server | notification |
| `prover/interpretToEnd` | client -> server | notification |
| `prover/stepForward` | client -> server | notification |
| `prover/stepBackward` | client -> server | notification |
| `prover/updateHighlights` | server -> client | notification |
| `prover/proofView` | server -> client | notification |
| `prover/moveCursor` | server -> client | notification |
| `prover/blockOnError` | server -> client | notification |
| `prover/searchResult` | server -> client | notification |
| `prover/debugMessage` | server -> client | notification |
| `prover/resetRocq` | client -> server | request |
| `prover/about` | client -> server | request |
| `prover/check` | client -> server | request |
| `prover/locate` | client -> server | request |
| `prover/print` | client -> server | request |
| `prover/search` | client -> server | request |
| `prover/documentState` | client -> server | request |
| `prover/documentProofs` | client -> server | request |

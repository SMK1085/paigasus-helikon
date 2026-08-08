# SMA-461 — AgentCore A2A and AG-UI protocol shims design

- **Ticket**: [SMA-461](https://linear.app/smaschek/issue/SMA-461/runtime-agentcore-a2a-and-ag-ui-protocol-shims)
- **Crate**: `paigasus-helikon-runtime-agentcore` (released, `0.2.0`)
- **Predecessor**: SMA-332 (shipped the HTTP and MCP modes; deferred these two in its §9)
- **Date**: 2026-08-08

## 1. Context and goal

AWS Bedrock AgentCore Runtime recognises four container protocols. SMA-332 implemented
two of them — HTTP (`0.0.0.0:8080`, `POST /invocations` + `GET /ping`) and MCP
(`0.0.0.0:8000`, `POST /mcp`) — and listed the other two as follow-ups. This ticket
implements them, plus the optional WebSocket endpoint the HTTP protocol allows:

1. **A2A** — `0.0.0.0:9000`, JSON-RPC 2.0 at the root path, with agent-card discovery.
2. **AG-UI** — `0.0.0.0:8080`, SSE at `/invocations` plus a WebSocket at `/ws`.
3. **`GET /ws`** on the existing HTTP protocol — optional per AWS's contract, unimplemented today.

The crate stays what it is: a *hosting shim*, not a `Runner`. Every mode delegates
execution to the configured `Runner` (`TokioRunner` by default) and reuses
`paigasus-helikon-runtime-axum`'s `SessionProvider`/`ContextProvider` traits, so all four
protocol modes share one provider vocabulary and one session story.

## 2. Verified facts this design rests on

Read from the AWS AgentCore developer guide on 2026-08-08, and from `cargo tree` in this
worktree. Facts that contradict or extend the ticket text are marked.

### 2.1 Protocol contracts (AWS docs)

| | HTTP | MCP | A2A | AG-UI |
| --- | --- | --- | --- | --- |
| Port | 8080 | 8000 | **9000** | **8080** |
| Mount path | `/invocations`, `/ws` | `/mcp` | `/` (root) | `/invocations` (SSE), `/ws` |
| Message format | REST JSON/SSE, WebSocket text/binary | JSON-RPC | JSON-RPC 2.0 | Event streams (SSE/WebSocket) |
| Discovery | n/a | tool listing | Agent Cards | n/a |

- **`GET /ping` is required by A2A and AG-UI too** — *extends the ticket*, which does not
  mention it. Both contract pages specify the identical body (`{"status":"Healthy"}` /
  `{"status":"HealthyBusy"}`, optional `time_of_last_update` in Unix seconds, set only on a
  genuine transition). This is exactly what `src/ping.rs`'s `PingState` already implements,
  so all four modes reuse it verbatim.
- **Session isolation** is the same header everywhere:
  `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id`, platform-injected. `src/session.rs`'s
  `extract_session_id` (33–256 chars) is reused unchanged.
- **A2A agent card** lives at `GET /.well-known/agent-card.json`. AWS's example pins
  `"protocolVersion": "0.3.0"`, `"preferredTransport": "JSONRPC"`, and carries
  `name`, `description`, `version`, `url`, `capabilities.streaming`,
  `defaultInputModes`, `defaultOutputModes`, `skills`.
- **A2A payloads pass through unmodified.** AWS describes its A2A support as "a transparent
  proxy layer": JSON-RPC payloads from `InvokeAgentRuntime` reach the container as sent.
- **AWS's SDK helper `serve_a2a`** handles "the `/ping` health endpoint, Agent Card serving,
  `AGENTCORE_RUNTIME_URL` environment variable, Bedrock header propagation, and runs on port
  9000 by default." That environment variable is how a deployed card learns its public URL.
- **AG-UI request body** is AG-UI's `RunAgentInput`:
  `{threadId, runId, messages, tools, context, state, forwardedProps}`. AWS explicitly notes
  it "passes request payloads directly to your container without validation" — the container
  decides what is required.
- **AG-UI response** is an SSE stream of `data: {"type":"…"}` frames; AWS's example shows
  `RUN_STARTED`, `TEXT_MESSAGE_START`, `TEXT_MESSAGE_CONTENT`, `TOOL_CALL_START`,
  `TOOL_CALL_RESULT`, `TEXT_MESSAGE_END`, `RUN_FINISHED`. Errors are a `RUN_ERROR` event.
- **HTTP `/ws`** takes a standard WebSocket upgrade carrying the session-id header, and may
  exchange text or binary messages; AWS recommends JSON. Both `/invocations` and `/ws` may be
  served from the same container on 8080.

### 2.2 The WebSocket quotas are a container obligation

AgentCore enforces a **64 KB message-frame size limit** and a **250 frames/second message-frame
rate limit**, and **closes the connection when either is exceeded**. AWS's guidance is to
"configure message frame fragmentation or implement chunking".

*Corrects the ticket*, which lists "64 KB frames, 250 fps" as if they were endpoint properties.
They are service quotas on our outbound traffic, and axum exposes no WebSocket fragmentation
API — `Message::Text` is one frame. Staying under both is therefore application-level work
this design must do explicitly (§7). Left unhandled, both fail *only in deployment*, as a
dropped connection with no local reproduction.

### 2.3 Dependency reality (`cargo tree`, this worktree)

- `axum`'s `ws` feature and `tokio-tungstenite 0.29` are **already** in
  `paigasus-helikon-runtime-agentcore`'s graph — pulled in unconditionally through
  `paigasus-helikon-runtime-axum`. WebSocket support therefore adds **no new crates**, and the
  30 MB image gate (currently met at 1.31 MB / 3.27 MB) is not in play.
- `jiff 0.2.28` (with `serde`) and `uuid 1.24` are likewise already in the graph. A2A needs
  RFC 3339 timestamps and unique task/context ids; both come free.
- **No credible Rust A2A or AG-UI crate exists.** Every candidate on crates.io is `0.0.x`/`0.1.x`
  with double-digit download counts (`a2a-core` 0.0.0 / 19 downloads; `agent-to-agent` 0.1.0 / 24;
  `ag-ui-protocol` 0.1.0 / 37; `ag-ui-server` 0.1.0 / 14). Against this workspace's `audit`/`deny`
  gates and a published SDK's stability promise, none is a defensible dependency. Both wire
  surfaces are small enough to hand-roll with `serde`, which is what this design does.

## 3. Approach

**Chosen: mode methods on the existing `AgentCoreServer`**, mirroring the `serve`/`serve_mcp`
pair SMA-332 established. Each protocol gets a pure `*_router()` (spawns nothing, testable with
`tower`'s `ServiceExt::oneshot`) and a `serve_*()` that binds the protocol's fixed port and logs
the same `"ready in {ms}ms"` cold-start line every existing mode logs.

```rust
server.serve().await?;        // HTTP,  0.0.0.0:8080  (existing; gains /ws)
server.serve_mcp().await?;    // MCP,   0.0.0.0:8000  (existing, unchanged)
server.serve_a2a().await?;    // A2A,   0.0.0.0:9000  (new)
server.serve_agui().await?;   // AG-UI, 0.0.0.0:8080  (new)
```

### Rejected alternatives

- **A `Protocol` enum with one `serve(protocol)` entry point.** Harder to mis-wire, but it
  breaks the published `serve()` signature, makes feature-gated variants awkward
  (`Protocol::A2a` must vanish without the `a2a` feature), and still needs the per-mode
  `Router` accessors the contract tests depend on. The breakage buys nothing the current shape
  lacks.
- **A distinct server type per protocol (`A2aServer`, `AgUiServer`).** Confines the A2A-only
  `task_store` setter at the type level, which is genuinely tidier — but it multiplies builders
  and docs and diverges from the crate's single-server design. The problem it solves is already
  solved here by `#[cfg(feature = …)]` on the setter, which is how the `mcp` methods are gated
  today.
- **Building A2A tasks on `runtime-axum`'s `EventLog`/`RunRegistry`.** Maximum reuse, but those
  types are keyed by run id and know nothing of `submitted`/`input-required`/`canceled`. Bending
  the A2A wire contract onto another crate's internal shape would couple them permanently and
  most likely force `runtime-axum` API changes (and therefore a manual version bump there).

## 4. Feature gating and module layout

### 4.1 Cargo features

All default-on, matching the existing `mcp` feature. Adding default features is not a semver
break, and the image-size headroom (§2.3) makes the default build's weight a non-issue.

```toml
[features]
default = ["mcp", "a2a", "ag-ui", "ws"]
mcp     = ["dep:paigasus-helikon-mcp", "dep:rmcp", "dep:async-trait"]
a2a     = ["dep:async-trait", "dep:uuid", "dep:jiff"]
ag-ui   = ["axum/ws", "dep:uuid"]
ws      = ["axum/ws"]
```

`runtime-agentcore` declares `axum/ws` **on its own dependency** rather than inheriting it from
`runtime-axum`'s feature unification. Today the two are indistinguishable at build time; they
stop being indistinguishable the moment `runtime-axum` drops `ws`, and the failure would be a
confusing compile error in an unrelated crate. This is the same class of latent bug as the
reqwest feature-unification trap recorded in the project's notes.

To keep that honest, the existing required `build-no-default-features` CI job gains one line:

```yaml
- run: cargo build -p paigasus-helikon-runtime-agentcore --no-default-features
```

This changes a required check's *content*, not its name, so branch protection is unaffected.

### 4.2 Module layout

`src/invoke.rs` is already 983 lines; nothing new lands in it.

```
src/
  frame.rs            [ws | ag-ui]  FrameBudget — shared quota enforcement (§7.1)
  ws.rs               [ws]          HTTP-mode GET /ws
  a2a/                [a2a]
    mod.rs                          a2a_router(), serve_a2a(), AppState wiring
    types.rs                        JSON-RPC envelope + A2A wire types (serde)
    card.rs                         AgentCard derivation and override
    rpc.rs                          method dispatch
    store.rs                        TaskStore trait + InMemoryTaskStore
  agui/               [ag-ui]
    mod.rs                          agui_router(), serve_agui()
    types.rs                        RunAgentInput + AG-UI event types
    map.rs                          AgentEvent -> AG-UI event mapping (§6.2)
    sse.rs                          POST /invocations
    ws.rs                           GET /ws
```

Unchanged and reused by every mode: `ping.rs` (`PingState`), `session.rs`
(`extract_session_id`), `server.rs`'s `AppState`/builder, `error.rs`.

## 5. A2A mode (feature `a2a`)

### 5.1 Endpoints

| Method | Path | Behaviour |
| --- | --- | --- |
| `POST` | `/` | JSON-RPC 2.0 dispatch (§5.3) |
| `GET` | `/.well-known/agent-card.json` | Agent card (§5.2) |
| `GET` | `/ping` | Existing `PingState` handler, verbatim |

`serve_a2a()` binds `0.0.0.0:9000`.

### 5.2 Agent card

Derived from the configured `Agent` so a correct card needs no extra configuration:

| Card field | Source |
| --- | --- |
| `name` | `Agent::name()` |
| `description` | `Agent::description()` |
| `version` | this crate's `CARGO_PKG_VERSION` by default — see the note below |
| `url` | `AGENTCORE_RUNTIME_URL` if set (what AWS's own `serve_a2a` does), else `http://0.0.0.0:9000/` |
| `protocolVersion` | `"0.3.0"` |
| `preferredTransport` | `"JSONRPC"` |
| `capabilities.streaming` | `true` |
| `defaultInputModes` / `defaultOutputModes` | `["text"]` |
| `skills` | one skill derived from the agent: `{id: name(), name: name(), description: description(), tags: []}` |

A single derived skill rather than `[]`: `skills` is what A2A clients discover on, and an empty
array is valid but useless. `AgentCoreServerBuilder::agent_card(AgentCard)` replaces the derived
card wholesale for callers who need real skill vocabulary.

**On `version`:** a library cannot read its *host binary's* version — `env!("CARGO_PKG_VERSION")`
resolves at compile time of the crate containing it, so inside this crate it always yields this
crate's version. The default is therefore this crate's version, which describes the shim rather
than the deployed agent. That is a defensible default (it is at least true) but rarely what a
caller wants published on a discovery card, so the derived-card docs say so plainly and point at
`.agent_card(...)` as the way to state the real agent version.

### 5.3 JSON-RPC methods

| Method | Transport | Behaviour |
| --- | --- | --- |
| `message/send` | JSON response | Buffered `Runner::run`; returns the completed `Task` |
| `message/stream` | SSE | `Runner::run_streamed`; status/artifact update events |
| `tasks/get` | JSON response | Store lookup |
| `tasks/cancel` | JSON response | Fires the run's `CancellationToken`; transitions to `canceled` |
| `tasks/resubscribe` | SSE | Replays `events_since(0)` then live-tails |
| `tasks/pushNotificationConfig/*` | JSON error | `-32003` PushNotificationNotSupported |
| `agent/getAuthenticatedExtendedCard` | JSON error | `-32004` UnsupportedOperation |
| anything else | JSON error | `-32601` MethodNotFound |

Request `parts` are text-only in v0; a `file`/`data` part answers `-32005`
ContentTypeNotSupported rather than being silently dropped.

Both streaming methods use the detached-driver + `CancellationToken` drop-guard pattern
`invoke.rs` established in SMA-332/SMA-456: the run is owned by a `tokio::spawn`ed task so a
client disconnect cannot cost the turn its finalize/session write, while the drop guard still
cancels the run promptly so a walked-away client stops burning tokens.

### 5.4 Task lifecycle and `contextId`

```
submitted ──> working ──> completed
                     ├──> failed        (RunFailed)
                     └──> canceled      (tasks/cancel, or client disconnect)
```

`input-required` is representable in the type but never produced in v0 — the agent loop has no
mid-run client-input suspension. Documented as such rather than omitted, because the state is
part of the A2A wire vocabulary a client may need to parse.

`contextId` binds to the AgentCore session id when the header is present, and is a fresh UUID
otherwise. That makes A2A's conversation grouping and our `Session` the same thing rather than
two parallel notions of "this conversation".

### 5.5 `TaskStore`

A public trait with a bounded in-memory default, wired through the builder exactly like
`session_provider` and `context_provider`:

```rust
#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn create(&self, task: Task) -> Result<(), AgentCoreError>;
    async fn get(&self, id: &str) -> Result<Option<Task>, AgentCoreError>;
    async fn update_state(&self, id: &str, state: TaskState) -> Result<(), AgentCoreError>;
    async fn append_event(&self, id: &str, event: TaskEvent) -> Result<u64, AgentCoreError>;
    async fn events_since(&self, id: &str, seq: u64) -> Result<Vec<TaskEvent>, AgentCoreError>;
}
```

Methods return `AgentCoreError`, not a new `A2aError`: a second error type buys nothing here, and
`AgentCoreError::Internal` already carries everything a store implementation needs to report.

`InMemoryTaskStore::new(capacity)` is the default (capacity 1024, LRU-evicting). It stores events
as well as state, because `tasks/resubscribe` must replay a task's history to a reconnecting
client.

**The durability gap is real and is documented, not papered over.** AgentCore documents no
`SIGTERM` contract, so an in-memory store dies with the microVM and a polling client sees its
task vanish mid-lifecycle. The trait *is* the answer: a deployment that needs durable tasks
supplies its own store. This crate ships no durable backend in this ticket.

### 5.6 Error codes — the trap on AWS's own page

AWS's A2A contract page publishes an error table (`-32051` ResourceNotFound, `-32052`
ValidationException, `-32053` Throttling, `-32054` Conflict, `-32055` RuntimeClientError). **Those
are codes the *platform* returns to a client on its own failures. They are not this container's
contract, and implementing them inside the container would be wrong.** It is the easiest mistake
to make from that page, so it is called out here and in the crate docs.

Our container emits **A2A-specification** codes:

| Code | Meaning | Emitted when |
| --- | --- | --- |
| `-32700` | Parse error | Body is not valid JSON |
| `-32600` | Invalid request | Not a valid JSON-RPC 2.0 envelope |
| `-32601` | Method not found | Unrecognised `method` |
| `-32602` | Invalid params | `params` fails to deserialize for a known method |
| `-32603` | Internal error | Run or store failure |
| `-32001` | TaskNotFound | `tasks/get`/`tasks/cancel`/`tasks/resubscribe` for an unknown id |
| `-32002` | TaskNotCancelable | `tasks/cancel` on an already-terminal task |
| `-32003` | PushNotificationNotSupported | Push-notification config methods |
| `-32004` | UnsupportedOperation | Extended-card method |
| `-32005` | ContentTypeNotSupported | Non-text message part |

Relatedly, AWS notes that the platform returns real HTTP status codes where the A2A spec puts
JSON-RPC errors on a `200`. That is also platform behaviour. **Our container follows the
specification**: a JSON-RPC error rides an HTTP `200`. Both distinctions go in the crate docs,
because a reader who lands on the AWS page first will otherwise conclude the implementation is
wrong.

## 6. AG-UI mode (feature `ag-ui`)

### 6.1 Endpoints

| Method | Path | Behaviour |
| --- | --- | --- |
| `POST` | `/invocations` | `RunAgentInput` in, AG-UI SSE event stream out |
| `GET` | `/ws` | Bidirectional AG-UI events (§7.3) |
| `GET` | `/ping` | Existing `PingState` handler, verbatim |

`serve_agui()` binds `0.0.0.0:8080`. AG-UI and the HTTP protocol are alternative
`serverProtocol` settings for one container, so the shared port is not a conflict: a given
deployment runs one or the other.

`RunAgentInput.messages` maps to `AgentInput`; `runId` is echoed in `RUN_STARTED`/`RUN_FINISHED`.
Fields this crate has no model for (`tools`, `context`, `state`, `forwardedProps`) are accepted
and ignored — AWS states the platform performs no validation, so rejecting them would be stricter
than the platform and would break compliant clients that always send them.

**Session precedence**: the platform-injected `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id`
header wins over the client-supplied `threadId`; `threadId` is used only when the header is
absent. Platform-authoritative beats client-supplied.

### 6.2 Event mapping

Native where AG-UI has a counterpart:

| `AgentEvent` | AG-UI event |
| --- | --- |
| `RunStarted` | `RUN_STARTED` |
| `TokenDelta` | `TEXT_MESSAGE_CONTENT` |
| `ReasoningDelta` | `THINKING_TEXT_MESSAGE_CONTENT` |
| `ToolCallDelta` | `TOOL_CALL_ARGS` |
| `ToolCallItem` | `TOOL_CALL_START` + `TOOL_CALL_END` |
| `ToolOutputItem` | `TOOL_CALL_RESULT` |
| `MessageOutput` | closes the open text run (`TEXT_MESSAGE_END`) |
| `TurnStarted` | `STEP_STARTED` |
| `RunCompleted` | `RUN_FINISHED` |
| `RunFailed` | `RUN_ERROR` |

The seven variants with no native AG-UI type are carried losslessly as AG-UI `CUSTOM` events under
a `helikon.` namespace, with the original event JSON as the value:

| `AgentEvent` | `CUSTOM` name |
| --- | --- |
| `GuardrailTriggered` | `helikon.guardrail` |
| `ApprovalRequested` | `helikon.approval` |
| `PermissionDenied` | `helikon.permission_denied` |
| `HandoffItem` | `helikon.handoff` |
| `AgentUpdated` | `helikon.agent_updated` |
| `RepairStarted` | `helikon.repair` |
| `StructuredOutputFailed` | `helikon.structured_output_failed` |

Dropping them was rejected: an `ApprovalRequested` that never reaches the client renders as a
silent stall, with no way for the frontend to recover it. `RAW` was rejected because it is
specified for upstream-provider passthrough, so frameworks surface it as opaque debug data — a UI
will not render an approval prompt out of it. A plain AG-UI frontend ignores unknown `CUSTOM`
names harmlessly; a Helikon-aware one can render them.

### 6.3 Text bracketing is the one stateful piece

`TokenDelta`/`ReasoningDelta` are bare fragments, but AG-UI requires balanced
`TEXT_MESSAGE_START` … `TEXT_MESSAGE_CONTENT` … `TEXT_MESSAGE_END` (and the `THINKING_*` triple).
So `agui/map.rs` holds a small state machine that:

- opens a text run on the first delta of a kind, allocating a message id;
- closes the open run when a delta of the *other* kind, a non-delta event, or the terminal arrives;
- guarantees no unbalanced pair is ever emitted, including on `RunFailed` mid-text.

Message ids are per-stream counters (`msg-0`, `msg-1`, …) rather than UUIDs: AG-UI only requires
uniqueness within the stream, and deterministic ids make the mapping tests assert exact frame
sequences. A2A task and context ids *are* UUIDs, because clients reference those across requests.

This state machine is the only component here with non-trivial invariants, so it is unit-tested
directly against `AgentEvent` sequences, independently of any transport.

### 6.4 Errors

A failure surfaces as a `RUN_ERROR` event, per AWS's contract. Once the stream has begun the HTTP
status stays `200` (SSE semantics — the same rule `invoke.rs` already follows); a failure *before*
the stream starts returns the real status with a `RUN_ERROR` body.

## 7. WebSocket, and the quotas that bite

### 7.1 `FrameBudget` (`src/frame.rs`)

Shared by both `/ws` endpoints. Enforces the two AgentCore quotas from §2.2 that would otherwise
manifest only in deployment, as a dropped connection:

- **Frame rate.** Token deltas are coalesced within a ~10 ms window, capping outbound frames at
  ~100/s against the 250/s limit. Coalescing concatenates consecutive same-kind text deltas into
  one frame; it never reorders and never merges across event kinds.
- **Frame size.** Any serialized payload above a conservative **60 KiB** threshold (against the
  64 KB limit) is split into continuation frames carrying a reassembly envelope:

  ```json
  {"type":"helikon.chunk","id":"c3","seq":0,"final":false,"data":"<partial JSON text>"}
  ```

  Frames **under** the threshold are sent bare — the plain event JSON, no envelope — so the common
  case is unpolluted and only oversize payloads pay the reassembly cost. The envelope's `type`
  shares the `helikon.` namespace used by the AG-UI `CUSTOM` events, so a client has one rule for
  "this is a Helikon extension".

The 60 KiB threshold and the 10 ms window are `const`s with the quota they defend named in a
comment, so a future quota change is a one-line edit against a stated source.

### 7.2 HTTP-mode `/ws` (feature `ws`, `src/ws.rs`)

Mounted on the existing `router()`, so `serve()` serves `/ping`, `/invocations`, and `/ws` from
one container on 8080 — which AWS's contract explicitly permits. Inbound text frames are parsed as
the **existing `InvocationRequest`**, so the WebSocket and `/invocations` speak the same request
vocabulary (all three body shapes) rather than inventing a second one. Outbound frames are
`AgentEvent` JSON through `FrameBudget`.

Session id comes from the header on the upgrade request, via the same `extract_session_id`.

**One run at a time per connection.** A new request arriving while a run is in flight cancels the
in-flight run and starts the new one — that is the "interactive agent sessions with user
interrupts" case AWS names for this endpoint. Binary frames are rejected with a close frame in v0;
the crate has no binary input model.

### 7.3 AG-UI `/ws` (`src/agui/ws.rs`)

Same upgrade, session, and `FrameBudget` plumbing; different vocabulary. Inbound frames are
`RunAgentInput`; outbound frames are AG-UI events through the §6.2 mapping. A separate handler
rather than a shared one, because AWS documents the two endpoints with genuinely different
message formats — sharing would mean a mode flag threaded through every branch.

## 8. Testing

House style: `Router::oneshot` contract tests in `#[cfg(test)]` modules beside the code.

**A2A** — agent-card shape and `AGENTCORE_RUNTIME_URL` override; `message/send` happy path
returning a completed `Task` with artifacts; `message/stream` frame sequence; `tasks/get` after a
send; `tasks/cancel` on an in-flight run transitioning to `canceled`; `tasks/cancel` on a terminal
task returning `-32002`; `tasks/resubscribe` replaying a completed task's events; unknown method
`-32601`; malformed body `-32700`; non-text part `-32005`.

**AG-UI** — full SSE frame sequence for a scripted agent; one test per `CUSTOM` mapping;
`RUN_ERROR` on run failure; header-beats-`threadId` precedence.

**Bracketing state machine** — driven directly with `AgentEvent` sequences, no transport:
interleaved token/reasoning deltas, a `RunFailed` mid-text, and a delta-free run all assert
balanced pairs.

**`FrameBudget`** — boundary tests at exactly 60 KiB and 60 KiB + 1 (bare vs. chunked), multi-chunk
reassembly, and a rate test asserting coalescing holds the frame rate under the cap.

**WebSocket** — upgrades cannot be `oneshot`-tested, so these bind an ephemeral listener and drive
it with `tokio-tungstenite` (already a `runtime-axum` dev-dep; added here as a dev-dep).

**Session isolation** — following the lesson recorded from SMA-482, isolation tests compare *full
responses* between a control and a test case rather than asserting a status code, so they cannot
pass against a broken implementation.

## 9. Documentation updated in the same PR

Per this repo's standing rules, all of these land on the feature branch:

- **`crates/paigasus-helikon-runtime-agentcore/README.md`** — contract tables for A2A, AG-UI and
  `/ws`; the new feature flags; the §5.6 error-code distinction; the CDK snippet gains the
  `ProtocolType.A2A` / `ProtocolType.AG_UI` variants alongside the existing `MCP` line.
- **`docs/book/src/concepts/runtimes.md`** — the "AgentCore recognizes two container protocols"
  table becomes four rows, with the `FrameBudget` quota note.
- **Crate docs (`src/lib.rs`)** — a section per new mode, mirroring the existing MCP-mode section,
  including the §5.6 trap and the §5.5 durability gap.
- **Root `README.md`** — the feature → module map, if the crate's feature list is reproduced there.
- **Examples** — `examples/a2a_server.rs` and `examples/agui_server.rs`, dependency-free like the
  existing `echo_http`, each with `required-features`.

## 10. Versioning and release

`paigasus-helikon-runtime-agentcore` is already released at `0.2.0`, so the normal release-plz flow
applies: **no manual version bump, no facade bump.** The manual-bump ritual exists only for stubs
ascending from `0.0.0` and for crates whose same-PR `core` API is needed at publish-verify time.

This design adds no `paigasus-helikon-core` API, so no `core` bump either. If implementation
discovers it needs one, the same-PR `core` + facade bump rule applies and this section is revised.

Commit scope: `runtime-agentcore` (in the `.versionrc` allowlist). PR title must carry a full
`type(scope):` prefix with a lowercase subject after the `SMA-461` token.

## 11. Out of scope

- **Durable `TaskStore` backends.** The trait is the seam; no SQLite/Postgres/Redis implementation
  ships here.
- **A2A push notifications** and the **authenticated extended card** — answered with explicit
  `-32003`/`-32004` rather than silence.
- **Non-text A2A parts** (`file`, `data`) — `-32005`.
- **AG-UI `STATE_SNAPSHOT`/`STATE_DELTA`** — the crate has no UI state model to synchronise.
- **Binary WebSocket frames** on either endpoint.
- **SigV4 / OAuth verification.** AgentCore terminates authentication before traffic reaches the
  container; this runtime exposes no `AuthLayer` seam, and `SessionKey::principal` stays `None`
  here for the reason already documented in the crate.
- **A live end-to-end test against a deployed AgentCore runtime.** Contract tests assert the wire
  shapes against the published contract; deploying is out of band, as it was for SMA-332.

## 12. Risks

| Risk | Mitigation |
| --- | --- |
| The 60 KiB chunking envelope is a Helikon invention, not an AgentCore or AG-UI concept | Only emitted above the threshold, namespaced `helikon.`, documented in the README contract table. A client that never receives an oversize payload never sees it. |
| Coalescing token deltas adds up to ~10 ms of latency | Bounded and constant; the alternative is a connection AgentCore closes outright. Threshold is a named `const`. |
| A2A 0.3.0 wire types hand-rolled from the spec could drift from real clients | Contract tests assert exact JSON against the shapes in AWS's published examples; the `a2a-inspector` tool AWS links is the manual cross-check. |
| Scope is large for one PR — five A2A modules with a public trait, plus AG-UI and two WebSocket endpoints | Flagged at design time; the split point (A2A first, AG-UI + `/ws` second) is clean, as they share only `PingState` and `FrameBudget`. Proceeding as one PR per the approved scope. |
| `TaskStore` is public API on a `0.x` crate | It mirrors `SessionProvider`/`ContextProvider`, which are already public and stable in shape; the risk is accepted deliberately as the answer to the durability gap. |

## 13. Acceptance criteria

1. `serve_a2a()` binds `0.0.0.0:9000` and serves `POST /`, `GET /.well-known/agent-card.json`, and
   `GET /ping`, with all five supported JSON-RPC methods and the documented error codes.
2. `serve_agui()` binds `0.0.0.0:8080` and serves `POST /invocations` (AG-UI SSE), `GET /ws`, and
   `GET /ping`, with the §6.2 mapping emitting balanced text pairs and losing no `AgentEvent`.
3. `serve()` additionally serves `GET /ws` under the `ws` feature, accepting the existing
   `InvocationRequest` shapes.
4. No outbound WebSocket frame exceeds 60 KiB, and the frame rate stays under the 250/s quota, both
   asserted by tests.
5. `cargo build -p paigasus-helikon-runtime-agentcore --no-default-features` succeeds, and is
   asserted by the `build-no-default-features` CI job.
6. All local CI gates pass: `fmt`, `clippy --workspace --all-features --all-targets -D warnings`,
   `test --workspace --all-features`, `doc` with `RUSTDOCFLAGS=-D warnings`, and doc coverage ≥ 80%.
7. README, book page, crate docs, and examples updated on the same branch.

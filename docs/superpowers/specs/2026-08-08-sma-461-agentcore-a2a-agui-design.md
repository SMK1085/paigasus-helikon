# SMA-461 — AgentCore A2A and AG-UI protocol shims design

- **Ticket**: [SMA-461](https://linear.app/smaschek/issue/SMA-461/runtime-agentcore-a2a-and-ag-ui-protocol-shims)
- **Crate**: `paigasus-helikon-runtime-agentcore` (released, `0.2.0`)
- **Predecessor**: SMA-332 (shipped the HTTP and MCP modes; deferred these two in its §9)
- **Date**: 2026-08-08
- **Revision**: 2 — revised after adversarial challenge; see §15 for the change log

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
protocol modes share one provider vocabulary.

## 2. Verified facts this design rests on

Read from the AWS AgentCore developer guide on 2026-08-08, and from this worktree's source
and `cargo tree`. Facts that contradict or extend the ticket text are marked.

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
  it "passes request payloads directly to your container without validation".
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
API — `Message::Text` is one frame. Staying under both is therefore application-level work this
design must do explicitly (§7). Left unhandled, both fail *only in deployment*, as a dropped
connection with no local reproduction.

**Two quota details AWS does not publish**, which the design must therefore assume
conservatively rather than tune against:

- Whether 250 fps is a one-second average or a shorter sliding window. §7.1 assumes the
  hostile reading (a short sliding window), so a burst cannot trip it even if the average is fine.
- Whether "64 KB" is 65 536 or 64 000 bytes. §7.1 budgets against **64 000**, the smaller reading.

Both assumptions are recorded next to the constants they justify, so a future AWS clarification
is a one-line edit against a stated source rather than an archaeology exercise.

### 2.3 Codebase facts verified in this worktree

- **`AgentEvent` is `#[non_exhaustive]` and has 17 variants** (`core/src/agent.rs:355`). Its own
  doc comment says "Fourteen variants" and is stale — noted here because the mapping in §6.2 must
  be complete against the *code*, not the comment, and because it is why §6.2 needs a wildcard arm
  with defined behaviour rather than an exhaustive `match`.
- **`ToolCallDelta` is emitted before `ToolCallItem`, not after.** Deltas are yielded while the
  *model stream* drains (`core/src/agent.rs:938`); the corresponding `ToolCallItem` is only
  pushed later by `transition()` (`core/src/loop_state.rs:330`) on the next loop iteration. Any
  mapping that treats `ToolCallItem` as the *start* of a tool call therefore emits its frames
  out of order. §6.2 is built around this.
- **`Runner::run` seeds the conversation as `history ++ input.messages`** (`core/src/runner.rs:70`),
  so `input` is *the new turn* and the session owns history. §6.1 depends on this.
- **`ParallelAgent`/`GraphAgent` forward branch events untagged.** `workflow.rs:347` merges
  branches with `select_all` over `(i, ev)` pairs, but the branch index is consumed for internal
  aggregation and discarded at `other => yield other` (`workflow.rs:390`). Downstream consumers
  cannot tell two concurrent branches' deltas apart. §6.3 documents the consequence.
- **`runtime-axum`'s `EventLog`** already solves replay-then-live-tail correctly: `Notify`-based
  wakeups, `notified().enable()` before `read_from` to close the lost-wakeup window, ring
  eviction with a `first_seq` cursor-clamp, and a regression test
  (`subscribe_does_not_lose_fast_appended_event`). §5.5 borrows the *algorithm*, not the type.
- **`AgentCoreError` has exactly two variants**, `BadRequest` and `Internal` (`error.rs:37,43`).
  "Not found" is not expressible today — §5.5 addresses this.
- **`mcp_router()` returns `Result<Router, AgentCoreError>`** while `router()` returns `Router`.
  The new accessors are infallible, so they return plain `Router`; the asymmetry is deliberate.

### 2.4 Dependency reality (`cargo tree`, this worktree)

- `axum`'s `ws` feature and `tokio-tungstenite 0.29` are **already** in this crate's graph, pulled
  unconditionally through `paigasus-helikon-runtime-axum`. WebSocket support adds **no new
  crates**, and the 30 MB image gate (currently met at 1.31 MB / 3.27 MB) is not in play.
- `jiff 0.2.28` (with `serde`) and `uuid 1.24` are likewise already in the graph. A2A needs
  RFC 3339 timestamps and unique task/context ids; both come free. The workspace `jiff` pin is
  an exact `=0.2.28` for a recorded reason — do not relax it as a side effect of this work.
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

- **A `Protocol` enum with one `serve(protocol)` entry point.** Harder to mis-wire, but it breaks
  the published `serve()` signature, makes feature-gated variants awkward (`Protocol::A2a` must
  vanish without the `a2a` feature), and still needs the per-mode `Router` accessors the contract
  tests depend on.
- **A distinct server type per protocol (`A2aServer`, `AgUiServer`).** Confines the A2A-only
  setters at the type level, which is tidier — but it multiplies builders and docs and diverges
  from the crate's single-server design. `#[cfg(feature = …)]` on the setters solves the same
  problem, and is how the `mcp` methods are gated today.
- **Building A2A tasks on `runtime-axum`'s `EventLog`/`RunRegistry` types.** Those are keyed by
  run id and know nothing of `submitted`/`input-required`/`canceled`. Bending the A2A wire
  contract onto another crate's internal shape would couple them permanently and most likely
  force `runtime-axum` API changes (and therefore a manual version bump there). This design
  reimplements `EventLog`'s *algorithm* inside `InMemoryTaskStore` (§5.5) rather than reusing the
  type — a deliberate, documented duplication of about 60 lines.

## 4. Feature gating and module layout

### 4.1 Cargo manifest changes

All new features default-on, matching the existing `mcp` feature. Adding default features is not
a semver break, and §2.4 shows no new crates enter the graph.

```toml
[features]
default = ["mcp", "a2a", "ag-ui", "ws"]
mcp     = ["dep:paigasus-helikon-mcp", "dep:rmcp", "dep:async-trait"]
a2a     = ["dep:async-trait", "dep:uuid", "dep:jiff"]
ag-ui   = ["axum/ws", "dep:uuid"]
ws      = ["axum/ws"]

[dependencies]
axum = { workspace = true, features = ["json"] }   # "ws" now comes from the features above
uuid = { workspace = true, optional = true }
jiff = { workspace = true, optional = true }

[dev-dependencies]
tokio-tungstenite = { workspace = true }
```

A facade user who wants only the HTTP contract opts out explicitly:
`paigasus-helikon-runtime-agentcore = { version = "…", default-features = false }`. Keeping the
four modes default-on matches `mcp`'s existing behaviour; making this one crate's protocol modes
opt-in while `mcp` stays opt-out would be a worse inconsistency than the compile time it saves.

**On declaring `axum/ws` here — an honest rationale.** `runtime-axum` is an *unconditional*
dependency of this crate and enables `axum/ws` unconditionally, so under feature unification
`axum/ws` is on in every build of this crate, including `--no-default-features`. **No CI check can
catch a missing declaration**, and the earlier draft of this spec wrongly claimed otherwise. The
declaration is still correct hygiene — this crate should not depend on another crate's feature
selection for code it compiles itself — but it is an *unguarded convention*, and is documented as
such so nobody later mistakes it for an enforced invariant.

The new CI line is kept for what it genuinely proves — that all four feature-gated module trees
compile out cleanly, which is exactly what the `#[cfg]` work in §4.2 can break:

```yaml
- run: cargo build -p paigasus-helikon-runtime-agentcore --no-default-features
```

This changes a required check's *content*, not its name, so branch protection is unaffected.

**Rejected:** splitting `ag-ui` into an SSE-only feature plus a separate `/ws` feature. AG-UI SSE
alone needs no WebSocket, but §2.4 shows tungstenite is unconditionally present regardless, so the
split saves nothing real while adding a third feature to document, gate, and test.

### 4.2 Module layout

`src/invoke.rs` is already 983 lines; nothing new lands in it.

```
src/
  frame.rs            cfg(any(ws, ag-ui))  FrameBudget — quota pacer (§7.1)
  ws.rs               cfg(ws)              HTTP-mode GET /ws
  a2a/                cfg(a2a)
    mod.rs                                 a2a_router(), serve_a2a()
    types.rs                               JSON-RPC envelope + A2A wire types (mostly pub(crate))
    card.rs                                AgentCard derivation and override
    rpc.rs                                 method dispatch
    store.rs                               TaskStore trait + InMemoryTaskStore
    cancel.rs                              live-run CancellationToken registry (§5.7)
  agui/               cfg(ag-ui)
    mod.rs                                 agui_router(), serve_agui()
    types.rs                               RunAgentInput + AG-UI event types (pub(crate))
    map.rs                                 AgentEvent -> AG-UI mapping + bracketing (§6.2, §6.3)
    sse.rs                                 POST /invocations
    ws.rs                                  GET /ws
```

**`server.rs` is *not* unchanged** — an earlier draft claimed it was, wrongly. Its deltas:

| Change | Gate |
| --- | --- |
| `AppStateInner` gains `tasks: Arc<dyn TaskStore>` and `cancels: Arc<CancelRegistry>` | `#[cfg(feature = "a2a")]` on the fields *and* their initializers |
| `AppStateInner` gains `card: Option<AgentCard>` | `#[cfg(feature = "a2a")]` |
| Builder gains `task_store()`, `agent_card()`, `agent_card_url()` | `#[cfg(feature = "a2a")]` |
| `router()` mounts `GET /ws` | `#[cfg(feature = "ws")]` around the single `.route(…)` |

Those `#[cfg]`s on struct fields are precisely what the `--no-default-features` build (AC5) exists
to catch, and are the most likely thing to get wrong.

Genuinely unchanged and reused by every mode: `ping.rs` (`PingState`), `session.rs`
(`extract_session_id`), `invoke.rs`, `mcp.rs`.

## 5. A2A mode (feature `a2a`)

### 5.1 Endpoints

| Method | Path | Behaviour |
| --- | --- | --- |
| `POST` | `/` | JSON-RPC 2.0 dispatch (§5.3) |
| `GET` | `/.well-known/agent-card.json` | Agent card (§5.2) |
| `GET` | `/ping` | Existing `PingState` handler, verbatim |

`serve_a2a()` binds `0.0.0.0:9000`; `a2a_router()` returns a plain `Router`.

### 5.2 Agent card

Derived from the configured `Agent` so a correct card needs no extra configuration:

| Card field | Source |
| --- | --- |
| `name` | `Agent::name()` |
| `description` | `Agent::description()` |
| `version` | this crate's `CARGO_PKG_VERSION` by default — see the note below |
| `url` | `AGENTCORE_RUNTIME_URL`, else `.agent_card_url(…)`, else the field is **omitted** |
| `protocolVersion` | `"0.3.0"` |
| `preferredTransport` | `"JSONRPC"` |
| `capabilities.streaming` | `true` |
| `defaultInputModes` / `defaultOutputModes` | `["text"]` |
| `skills` | one skill derived from the agent: `{id: name(), name: name(), description: description(), tags: []}` |

A single derived skill rather than `[]`: `skills` is what A2A clients discover on, and an empty
array is valid but useless. `AgentCoreServerBuilder::agent_card(AgentCard)` replaces the derived
card wholesale for callers who need real skill vocabulary.

**On `url`:** publishing `http://0.0.0.0:9000/` — an earlier draft's fallback — would be actively
misleading, since `0.0.0.0` is a bind address and not routable. The field is omitted instead when
nothing authoritative is known. Because reading `AGENTCORE_RUNTIME_URL` is a hidden global input
in a codebase that otherwise favours explicit injection, `.agent_card_url(…)` exists as the
explicit path and the env var is documented as the deployed-on-AgentCore convenience it is.

**On `version`:** a library cannot read its *host binary's* version — `env!("CARGO_PKG_VERSION")`
resolves at compile time of the crate containing it, so inside this crate it always yields this
crate's version. The default therefore describes the shim, not the deployed agent. That is at
least true, but rarely what a caller wants on a discovery card, so the docs say so plainly and
point at `.agent_card(…)`.

### 5.3 JSON-RPC methods

| Method | Transport | Behaviour |
| --- | --- | --- |
| `message/send` | JSON response | Buffered `Runner::run`; returns the completed `Task` |
| `message/stream` | SSE | `Runner::run_streamed`; status/artifact update events |
| `tasks/get` | JSON response | Store lookup |
| `tasks/cancel` | JSON response | §5.7 |
| `tasks/resubscribe` | SSE | `TaskStore::subscribe` — replay then live-tail (§5.5) |
| push-notification config methods | JSON error | `-32003` PushNotificationNotSupported |
| authenticated extended card | JSON error | `-32004` UnsupportedOperation |
| anything else | JSON error | `-32601` MethodNotFound |

**Method-name strings are pinned at implementation time from the A2A 0.3.0 specification**, not
from memory or from this table's prose. The exact spellings for the push-notification and
extended-card families are ambiguous across sources (`agent/authenticatedExtendedCard` vs
`agent/getAuthenticatedExtendedCard`), and because the fallthrough is a silent `-32601`, a
mismatch is invisible in testing. The implementation records the cited source in a comment.

**Inbound `taskId` / `contextId` on `message/send` and `message/stream`.** A real A2A client sends
`message.taskId` to continue an existing task and `message.contextId` to join a conversation, so
these cannot be ignored:

| Inbound | Behaviour |
| --- | --- |
| `taskId` absent | Mint a new task (the common case) |
| `taskId` names a known, non-terminal task | Continue it: new events append to the same task |
| `taskId` names a known, terminal task | `-32602` invalid params (a terminal task cannot take more input) |
| `taskId` names an unknown task | `-32001` TaskNotFound |
| `contextId` present and the session header is absent | Use it as the context |
| `contextId` present and disagrees with the session header | **Header wins**, mirroring §6.1 — platform-authoritative beats client-supplied |

Request `parts` are text-only in v0; a `file`/`data` part answers `-32005` ContentTypeNotSupported
rather than being silently dropped.

### 5.4 Task lifecycle, `contextId`, and what a disconnect means

```
submitted ──> working ──> completed
                     ├──> failed        (RunFailed)
                     └──> canceled      (tasks/cancel only — see below)
```

`input-required` is representable in the type but never produced in v0 — the agent loop has no
mid-run client-input suspension. Documented rather than omitted, because a client may parse it.

**A client disconnect does *not* cancel an A2A task.** This is the opposite of `/invocations`'
HTTP behaviour, and an earlier draft of this spec had it wrong in a way that would have made
`tasks/resubscribe` useless — a resubscribing client could only ever find a cancelled task.
Resubscription exists precisely so a dropped stream can be reattached, so on A2A:

- the detached driver keeps running the task to its terminal and keeps appending to the store;
- the task stays `working` and is reachable by `tasks/get` and `tasks/resubscribe`;
- **only `tasks/cancel` produces `canceled`**.

The detached-driver pattern from SMA-456 still applies (the run is owned by a `tokio::spawn`ed
task, so finalize and the session write always happen). What does *not* apply on A2A is the
`CancellationToken` drop-guard that `invoke.rs` binds to the response's lifetime: binding it here
would cancel the task on exactly the disconnect that resubscribe is meant to survive.

`contextId` binds to the AgentCore session id when the header is present, and is a fresh UUID
otherwise. That makes A2A's conversation grouping and our `Session` the same thing rather than
two parallel notions. Note this echoes a platform-issued identifier back to A2A clients; that is
acceptable because AgentCore terminates authentication before traffic reaches the container, but
it is a deliberate choice rather than an accident.

### 5.5 `TaskStore`

A public trait with a bounded in-memory default, wired through the builder like
`session_provider` and `context_provider`:

```rust
#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn create(&self, task: Task) -> Result<(), AgentCoreError>;
    async fn get(&self, id: &str) -> Result<Option<Task>, AgentCoreError>;

    /// Compare-and-swap. `Ok(false)` when the task's current state is not `expected`.
    async fn update_state(
        &self,
        id: &str,
        expected: TaskState,
        new: TaskState,
    ) -> Result<bool, AgentCoreError>;

    /// Returns the sequence number **assigned to this event**.
    async fn append_event(&self, id: &str, event: TaskEvent) -> Result<u64, AgentCoreError>;

    /// Replace a task's artifacts, so a later `tasks/get` reports the same output the
    /// original `message/send` returned.
    async fn set_artifacts(
        &self,
        id: &str,
        artifacts: Vec<Artifact>,
    ) -> Result<(), AgentCoreError>;

    /// Replay from `from` (**inclusive**), then live-tail until the task is terminal.
    async fn subscribe(
        &self,
        id: &str,
        from: u64,
    ) -> Result<BoxStream<'static, TaskEvent>, AgentCoreError>;
}
```

**`set_artifacts` was added during implementation.** `message/send` returns the finished `Task`,
and that task has to carry its artifacts; none of the other five methods can write them, so
`tasks/get` would otherwise report an artifact-less copy of the task the caller had just been
handed. It is part of the trait a custom durable store must implement.

**`subscribe` replaces the earlier draft's `events_since`**, which was a snapshot read with no
wakeup primitive — "replay then live-tail" had no mechanism behind it, and any implementation
would have been forced into polling with an unavoidable drop/duplicate window between the last
read and the live attach. The contract is: *every* event from `from` onward is delivered exactly
once and in order, with no gap at the replay/live seam, and the stream ends when the task reaches
a terminal state. `InMemoryTaskStore` implements this with `EventLog`'s algorithm (§2.3):
`Notify`, with `notified().enable()` called *before* the backlog read so a concurrent append
cannot be lost in the window between them.

**Defined behaviour, so two implementations cannot diverge:**

| Situation | Result |
| --- | --- |
| `get` on an unknown id | `Ok(None)` |
| `update_state` / `append_event` / `subscribe` on an unknown id | `Err(AgentCoreError::NotFound)` |
| `update_state` where current state ≠ `expected` | `Ok(false)` — caller decides |
| `subscribe` with a `from` older than the oldest retained event | Stream starts at the oldest retained event; the gap is logged, not silently hidden |

This requires a **third `AgentCoreError` variant, `NotFound`** (HTTP 404). `AgentCoreError` is
`#[non_exhaustive]`, so this is additive. It is the only `core`-adjacent type change in the ticket
and it is local to this crate.

`InMemoryTaskStore::new(capacity)` is the default: **1024 tasks** (LRU-evicting) and **512 events
per task** (ring buffer with a `first_seq` cursor-clamp, exactly like `EventLog`). The per-task cap
matters — an earlier draft bounded only the task count, so a single long streaming run grew
without limit.

**Atomicity.** `update_state` and `append_event` remain separate calls, so a concurrent `tasks/get`
can observe a state that lags the event log by one call. That is accepted rather than solved with a
transaction API: the states are monotonic, the window is one `await`, and the alternative is a
much heavier trait for a store whose default is a single in-process mutex. It is documented on the
trait so a distributed implementation knows what guarantee it is (not) required to provide.

**The durability gap is real and is documented, not papered over.** AgentCore documents no
`SIGTERM` contract, so an in-memory store dies with the microVM and a polling client sees its task
vanish. The trait *is* the answer: a deployment needing durable tasks supplies its own store. This
crate ships no durable backend here. See §11 for what a durable store does *not* buy you.

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
| `-32602` | Invalid params | `params` fails to deserialize; continuation of a terminal task |
| `-32603` | Internal error | Run or store failure |
| `-32001` | TaskNotFound | Unknown task id on `tasks/*` or an inbound `taskId` |
| `-32002` | TaskNotCancelable | `tasks/cancel` on an already-terminal task, or no live run (§5.7) |
| `-32003` | PushNotificationNotSupported | Push-notification config methods |
| `-32004` | UnsupportedOperation | Extended-card method |
| `-32005` | ContentTypeNotSupported | Non-text message part |

Relatedly, AWS notes that the platform returns real HTTP status codes where the A2A spec puts
JSON-RPC errors on a `200`. That is also platform behaviour. **Our container follows the
specification**: a JSON-RPC error rides an HTTP `200`. Both distinctions go in the crate docs,
because a reader who lands on the AWS page first will otherwise conclude the implementation is
wrong.

### 5.7 `tasks/cancel` and the live-run registry

An earlier draft said `tasks/cancel` "fires the run's `CancellationToken`" without saying where a
live run's token lives or how it is reached from a task id — leaving the method with no mechanism
at all. It gets one:

`a2a/cancel.rs` holds a `CancelRegistry`: a `Mutex<HashMap<TaskId, CancellationToken>>` in
`AppState`. A token is registered when the run is spawned and removed by the *same* detached-task
path that already owns the run's lifetime in `invoke.rs`, so the map cannot outlive its runs.

| `tasks/cancel` case | Result |
| --- | --- |
| Task is live in this container | Fire the token; then CAS `working → canceled` |
| Task is known but already terminal | `-32002` TaskNotCancelable |
| Task is known but has no live token (durable store, different microVM) | `-32002`, with a message naming the reason |
| Task is unknown | `-32001` TaskNotFound |

**The cancel-vs-completion race is resolved by the CAS, not by ordering.** `Runner::run`'s
documented precedence is that a genuine terminal event that already occurred beats a late cancel.
So the cancel path issues `update_state(id, expected: Working, new: Canceled)` and *honours a
`false` return*: the driver already wrote `completed`/`failed`, that outcome stands, and the RPC
reports `-32002`. Blind overwrite — the earlier draft's implicit model — would have let a losing
cancel rewrite a completed task's state.

## 6. AG-UI mode (feature `ag-ui`)

### 6.1 Endpoints and the session model

| Method | Path | Behaviour |
| --- | --- | --- |
| `POST` | `/invocations` | `RunAgentInput` in, AG-UI SSE event stream out |
| `GET` | `/ws` | Bidirectional AG-UI events (§7.3) |
| `GET` | `/ping` | Existing `PingState` handler, verbatim |

`serve_agui()` binds `0.0.0.0:8080`. AG-UI and the HTTP protocol are alternative `serverProtocol`
settings for one container, so the shared port is not a conflict: a deployment runs one or the
other. `/invocations` here is **SSE only** — it does not honour `Accept: application/json` the way
HTTP mode's does, because the AG-UI contract defines no buffered form.

**AG-UI mode is stateless per request, and this is the design's most important correction.**
`RunAgentInput.messages` carries the *entire* conversation on every request — that is AG-UI's
model, where the client owns thread state. But `Runner::run` seeds `history ++ input.messages`
(§2.3), so combining a persisted session with a full client-supplied history double-counts every
prior turn: turn 2 would hand the model `[u1, a1] ++ [u1, a1, u2]`, and the corruption compounds
with each turn. The earlier draft did exactly this.

So AG-UI mode resolves a **fresh, unshared session per request** and treats `messages` as the
whole conversation. This is not a novel rule — it is precisely what MCP mode in this crate already
does (a fresh in-memory session per call), and it is documented the same way: **AG-UI mode cannot
use a persistent session backend in v0**, and the configured `SessionProvider` is not consulted.
`threadId` and the session header are carried into the `RunContext` for observability and
isolation, not for persistence.

`runId` is echoed in `RUN_STARTED`/`RUN_FINISHED`. Fields this crate has no model for (`tools`,
`context`, `state`, `forwardedProps`) are accepted and ignored — **for client compatibility**:
compliant AG-UI clients always send them, so rejecting them would break every real client. The
`tools` case is called out loudly in the docs, because a client that registers frontend tools will
wait for `TOOL_CALL_*` frames this runtime never produces for them, and a silent ignore looks like
a hang.

A deployment that sets `serverProtocol: HTTP` but runs `serve_agui()` gets a working transport
with a silently wrong event vocabulary on the same `/invocations` path. Both `serve()` and
`serve_agui()` document this; the 8080 bind-failure message names it as the likely cause.

### 6.2 Event mapping

Native where AG-UI has a counterpart. **Tool-call bracketing is derived from the deltas, not from
`ToolCallItem`** — §2.3 establishes that deltas arrive *first*, so the earlier draft's mapping
(`ToolCallItem` → `TOOL_CALL_START`) would have emitted `TOOL_CALL_ARGS` for a `toolCallId` the
client had never seen started, which AG-UI's reference client rejects:

| `AgentEvent` | AG-UI event |
| --- | --- |
| `RunStarted` | `RUN_STARTED` |
| `TurnStarted` | `STEP_STARTED` (bracketed — §6.3) |
| `TokenDelta` | `TEXT_MESSAGE_CONTENT` (bracketed) |
| `ReasoningDelta` | `THINKING_TEXT_MESSAGE_CONTENT` (bracketed) |
| `ToolCallDelta`, **first for a `call_id`** | `TOOL_CALL_START` (its `name` is `Some` only on the first delta — exactly the START payload) + `TOOL_CALL_ARGS` |
| `ToolCallDelta`, subsequent | `TOOL_CALL_ARGS` |
| `ToolCallItem` | `TOOL_CALL_END`; **or** the full `START`+`ARGS`+`END` triple if no deltas were seen for that `call_id` (non-streaming providers) |
| `ToolOutputItem` | `TOOL_CALL_RESULT` |
| `MessageOutput` | closes an open text run, **or** synthesizes a full text triple — see below |
| `RunCompleted` | `RUN_FINISHED` |
| `RunFailed` | `RUN_ERROR` |

**`MessageOutput` with no preceding deltas must synthesize text, not just close.** A
non-streaming provider, a workflow agent, or the crate's own `EchoAgent` fixtures emit
`RunStarted → MessageOutput → RunCompleted` with zero deltas. The earlier draft only *closed* an
open run on `MessageOutput`, so those agents produced a stream with no `TEXT_MESSAGE_*` at all —
a blank UI — and the planned "delta-free run" test asserted *balance*, which an empty stream
trivially satisfies. So: when `MessageOutput` arrives with no open text run, emit
`TEXT_MESSAGE_START` + one `TEXT_MESSAGE_CONTENT` carrying the item's text + `TEXT_MESSAGE_END`.
The test asserts the text is **present**, not merely balanced.

The seven variants with no native AG-UI type are carried losslessly as `CUSTOM` events under a
`helikon.` namespace, with the original event JSON as the value:

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
silent stall. `RAW` was rejected because it is specified for upstream-provider passthrough, so
frameworks surface it as opaque debug data — a UI will not render an approval prompt out of it. A
plain AG-UI frontend ignores unknown `CUSTOM` names harmlessly.

**The wildcard arm has defined behaviour.** `AgentEvent` is `#[non_exhaustive]` (§2.3), so the
`match` cannot be exhaustive and a variant added to `core` later would otherwise vanish silently.
Unknown variants map to `CUSTOM` named `helikon.unknown` carrying `serde_json::to_value(&event)` —
lossless, consistent with the rationale above, and degrading rather than dropping. A test
constructs every variant's serialized form and asserts each maps to *something*, so the count is
visible in review even though the compiler cannot enforce it.

### 6.3 Bracketing: the one stateful piece

`agui/map.rs` owns a state machine over **all** paired AG-UI event families, not just text — the
earlier draft scoped it to text and thinking, which left `STEP_STARTED` permanently unmatched
because no `AgentEvent` means "turn finished":

| Pair | Opened by | Closed by |
| --- | --- | --- |
| `TEXT_MESSAGE_START` / `END` | first `TokenDelta` | a non-text event, a `MessageOutput`, or the terminal |
| `THINKING_START` / `END` | first `ReasoningDelta` | a non-reasoning event or the terminal |
| `TOOL_CALL_START` / `END` | first `ToolCallDelta` for a `call_id` | matching `ToolCallItem`, or the terminal |
| `STEP_STARTED` / `STEP_FINISHED` | `TurnStarted` | the next `TurnStarted`, or the terminal |

**Invariant, restated to cover every family:** every pair the mapper opens is closed exactly once,
including on `RunFailed` mid-stream, and no `*_END` is emitted without a matching `*_START`. The
terminal handler flushes all open pairs before emitting `RUN_FINISHED`/`RUN_ERROR`.

Message ids are per-stream counters (`msg-0`, `msg-1`, …) rather than UUIDs: AG-UI needs only
stream-local uniqueness, and deterministic ids let the tests assert exact frame sequences. A2A
task and context ids *are* UUIDs, because clients reference those across requests.

**Known limitation — concurrent agents.** §2.3 establishes that `ParallelAgent`/`GraphAgent`
forward branch events untagged, so two branches' `TokenDelta`s interleave into one flat stream
with nothing to tell them apart, and this state machine will concatenate them into a single
garbled message. Fixing it properly needs a branch discriminator on the delta variants — a `core`
API change, which is out of scope here (§11). The limitation is documented in the crate docs and
the book, not left to be discovered.

### 6.4 Errors and disconnect

A failure surfaces as a `RUN_ERROR` event, per AWS's contract. Once the stream has begun the HTTP
status stays `200` (SSE semantics — the rule `invoke.rs` already follows); a failure *before* the
stream starts returns the real status with a `RUN_ERROR` body.

**AG-UI's SSE path reuses `invoke.rs`'s disconnect pattern exactly** — detached driver plus a
`CancellationToken` drop-guard bound to the response — and the earlier draft's silence on this was
an omission that would have regressed the precise bug SMA-456 fixed. Unlike A2A (§5.4), the
drop-guard *does* apply here: AG-UI has no resubscribe, so a departed client should stop the run.
§8 adds the equivalent of `sse_client_disconnect_still_finalizes_the_session`.

## 7. WebSocket, and the quotas that bite

### 7.1 `FrameBudget` (`src/frame.rs`)

Shared by both `/ws` endpoints (A2A has no WebSocket and does not use it). Enforces §2.2's quotas,
which otherwise manifest only in deployment, as a dropped connection.

**A rate *pacer*, not just coalescing.** The earlier draft coalesced consecutive text deltas and
called that rate limiting. It is not: `TOOL_CALL_ARGS`, `TOOL_CALL_RESULT`, the `CUSTOM` events and
`STEP_*` were all unbounded, so a parallel-tool-call turn could burst past 250 fps with no text
involved at all. `FrameBudget` is a **token bucket over every outbound frame**, sized against
§2.2's hostile assumptions (a short sliding window; 64 000-byte frames). Coalescing consecutive
same-kind text deltas remains, but as an *optimization inside* the bucket that reduces frame count
— never as the mechanism that enforces the limit. Coalescing never reorders and never merges
across event kinds.

**Size: content-level splitting first, envelope only as a last resort.** The earlier draft wrapped
any oversize payload in a `helikon.chunk` envelope that *replaced* the frame. Three problems, all
deployment-only: a client without reassembly logic loses the whole event (and if that event is a
`*_END`, §6.3's invariant is violated as *observed by the client*, which is the only place it
matters); the threshold was measured on the payload rather than the serialized frame, but JSON
escaping expands (a control character becomes six bytes), so an under-threshold payload can
serialize over the limit; and splitting JSON text at a byte offset can land mid-codepoint,
producing invalid UTF-8.

So:

1. **Measure `serde_json::to_string(&frame).len()`** — the actual bytes on the wire — against a
   conservative **60 000-byte** threshold (against §2.2's 64 000 assumption).
2. **Split at the content level where the protocol allows it**, which needs no envelope and keeps
   every client working: an oversize `TEXT_MESSAGE_CONTENT` becomes several valid
   `TEXT_MESSAGE_CONTENT` frames; likewise `TOOL_CALL_ARGS`. Splits land on `char_indices`
   boundaries, never mid-codepoint.
3. **Only for events that genuinely cannot be split** — `TOOL_CALL_RESULT`, `CUSTOM` — fall back
   to the `helikon.chunk` envelope, and document that exact list so a client author knows the
   finite set of frames needing reassembly.

Thresholds and the window are `const`s with the quota and the §2.2 assumption named in a comment,
so a future AWS clarification is a one-line edit.

### 7.2 HTTP-mode `/ws` (feature `ws`, `src/ws.rs`)

Mounted on the existing `router()`, so `serve()` serves `/ping`, `/invocations`, and `/ws` from one
container on 8080 — which AWS's contract explicitly permits. Inbound text frames are parsed as the
**existing `InvocationRequest`**, so the WebSocket and `/invocations` speak the same request
vocabulary (all three body shapes). Outbound frames are `AgentEvent` JSON through `FrameBudget`.

Session id comes from the header on the upgrade request, via `extract_session_id`.

**One `RunContext` and one `CancellationToken` per inbound run — not per connection.**
`CancellationToken` is one-shot: a context built once at upgrade time and reused would leave run B
starting already-cancelled after run A was interrupted, breaking the *second* message on every
connection. `ContextProvider::build` is therefore called per run, reusing the upgrade request's
`Parts` and the resolved session, with a fresh token each time.

**One run at a time, and the interrupt waits for the previous finalize.** A new request while a run
is in flight cancels the in-flight run — the "interactive sessions with user interrupts" case AWS
names — and then **awaits that run's detached task before starting the next**. Without the await,
run B could load session history before run A's finalize lands, so A's partial turn would be
missing from B's context or appended after it. `Runner::run` issues the session write before it
resolves, so awaiting the task is a sufficient ordering barrier.

Inbound frames are capped at **2 MiB**, matching `/invocations`' existing `MAX_BODY_BYTES`, rather
than inheriting axum's default. Binary frames are rejected in v0 with close code **1003**
(Unsupported Data); the crate has no binary input model.

### 7.3 AG-UI `/ws` (`src/agui/ws.rs`)

Same upgrade, session, per-run-context, interrupt-ordering, size-cap and `FrameBudget` plumbing;
different vocabulary. Inbound frames are `RunAgentInput`; outbound frames are AG-UI events through
the §6.2 mapping and the §6.3 state machine. A separate handler rather than a shared one, because
AWS documents the two endpoints with genuinely different message formats — sharing would mean a
mode flag threaded through every branch.

## 8. Testing

House style: `Router::oneshot` contract tests in `#[cfg(test)]` modules beside the code. All test
modules are feature-gated so `cargo test --no-default-features` still compiles.

**A2A** — agent-card shape, `AGENTCORE_RUNTIME_URL` override, and `url` omission when neither
source is set; `message/send` returning a completed `Task` with artifacts; `message/stream` frame
sequence; inbound `taskId` continuation, terminal-task `-32602`, unknown-task `-32001`;
`tasks/get` after a send; `tasks/cancel` on a live run; `tasks/cancel` losing the CAS race against
a completed run returning `-32002` **and leaving the state `completed`**; `tasks/cancel` with no
live token; `tasks/resubscribe` replaying a completed task; **`tasks/resubscribe` attaching mid-run
and receiving both backlog and live events with no gap or duplicate** (the `EventLog` lost-wakeup
scenario); **an SSE disconnect leaving the task `working` and resubscribable** (§5.4); unknown
method `-32601`; malformed body `-32700`; non-text part `-32005`.

**AG-UI** — full SSE frame sequence for a scripted agent; **the tool-call ordering test driven by a
recorded real `LlmAgent` event sequence, not a hand-written one** (a hand-scripted agent would
encode whatever order the author assumed and pass either way — the §6.2 bug was exactly this);
`MessageOutput`-only agent producing *visible* text; one test per `CUSTOM` mapping; a
serialize-every-variant test asserting the wildcard arm is reached rather than dropping;
`RUN_ERROR` on failure; **turn-2 message count asserting no double-counting** (§6.1), mirroring
`invoke.rs`'s existing `same_session_id_continues_the_conversation`; SSE-disconnect-still-finalizes.

**Bracketing state machine** — driven directly with `AgentEvent` sequences, no transport:
interleaved token/reasoning deltas, multi-turn `STEP_*` balance, tool-call pairs, `RunFailed`
mid-text, and a delta-free run. Asserts every opened pair closes exactly once across all four
families.

**`FrameBudget`** — **deterministic, with an injectable clock (`tokio::time::pause`)**, asserting
the emitted frame count for a synthetic event sequence rather than wall-clock rate. A wall-clock
timing assertion across `{ubuntu, macos, windows} × {stable, 1.94}` — two of which are required
contexts — is exactly the flake this repo has been bitten by before. Plus: boundary tests at
exactly 60 000 and 60 001 serialized bytes; a payload whose JSON escaping pushes it over the
threshold although its raw length is under; multi-byte-codepoint splits asserting valid UTF-8;
content-level splitting of `TEXT_MESSAGE_CONTENT`; envelope fallback for `TOOL_CALL_RESULT`.

**WebSocket** — upgrades cannot be `oneshot`-tested, so these bind an ephemeral listener and drive
it with `tokio-tungstenite`. Cases: two sequential requests on one connection **both completing**
(the one-shot-token bug); an interrupt whose successor sees the interrupted turn in context (the
finalize-ordering bug); a binary frame closing with 1003; an oversize inbound frame rejected.

**Session isolation** — following the lesson from SMA-482, isolation tests compare *full responses*
between a control and a test case rather than asserting a status code, so they cannot pass against
a broken implementation.

## 9. Documentation updated in the same PR

Per this repo's standing rules, all of these land on the feature branch:

- **`crates/paigasus-helikon-runtime-agentcore/README.md`** — contract tables for A2A, AG-UI and
  `/ws`; the new feature flags and the `default-features = false` opt-out; the §5.6 error-code
  distinction; the §6.1 AG-UI stateless-session limitation; the finite list of chunk-envelope
  event types (§7.1); the CDK snippet's `ProtocolType.A2A` / `ProtocolType.AG_UI` variants.
- **`docs/book/src/concepts/runtimes.md`** — the "AgentCore recognizes two container protocols"
  table becomes four rows, plus the `FrameBudget` quota note and the §6.3 concurrent-agent
  limitation.
- **Crate docs (`src/lib.rs`)** — a section per new mode, mirroring the existing MCP-mode section,
  including the §5.6 trap, the §5.5 durability gap, and the §6.1 session model.
- **`crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile`** — its `EXPOSE`/comment block
  documents "either mode's contract" and needs port 9000 added.
- **`crates/paigasus-helikon/README.md`** (facade) and root **`README.md`** — CLAUDE.md names both
  whenever the feature → module map changes.
- **Examples** — `examples/a2a_server.rs` and `examples/agui_server.rs`, dependency-free like the
  existing `echo_http`, each with `required-features`.

**Doc-coverage budget.** `TaskStore`'s signature forces `Task`, `TaskState`, `TaskEvent` and
`AgentCard` public; every field needs a `///` or the required `docs` job fails under
`RUSTDOCFLAGS=-D warnings`, and `doc-coverage` is gated at 80%. Everything *not* appearing in a
public signature stays `pub(crate)` — the JSON-RPC envelope, `RunAgentInput`, and the whole AG-UI
event enum — which is both cheaper and better encapsulation.

## 10. Versioning and release

`paigasus-helikon-runtime-agentcore` is already released at `0.2.0`, so the normal release-plz flow
applies: **no manual version bump, no facade bump.** The manual-bump ritual exists only for stubs
ascending from `0.0.0` and for crates whose same-PR `core` API is needed at publish-verify time.

This design adds no `paigasus-helikon-core` API (the new `AgentCoreError::NotFound` variant is
local to this crate). If implementation discovers it needs a `core` change, the same-PR `core` +
facade bump rule applies and this section is revised.

Commit scope: `runtime-agentcore` (in the `.versionrc` allowlist). PR title must carry a full
`type(scope):` prefix with a lowercase subject after the `SMA-461` token.

## 11. Out of scope

- **Durable `TaskStore` backends.** The trait is the seam; no SQLite/Postgres/Redis implementation
  ships here. Note that a durable store does *not* make A2A fully container-independent: a
  `tasks/cancel` reaching a container that never ran the task has no token to fire (§5.7), and
  `tasks/resubscribe` there can replay history but cannot live-tail a run happening elsewhere.
  Cross-container task control is out of scope.
- **Branch-tagged events for `ParallelAgent`/`GraphAgent`** (§6.3) — needs a `core` API change.
- **A2A push notifications** and the **authenticated extended card** — explicit `-32003`/`-32004`.
- **Non-text A2A parts** (`file`, `data`) — `-32005`.
- **AG-UI `STATE_SNAPSHOT`/`STATE_DELTA`** and frontend `tools` — no UI state model (§6.1).
- **A persistent session backend in AG-UI mode** (§6.1) — a v0 limitation, like MCP mode's.
- **Binary WebSocket frames** on either endpoint.
- **SigV4 / OAuth verification.** AgentCore terminates authentication before traffic reaches the
  container; `SessionKey::principal` stays `None` here for the reason already documented.
- **A live end-to-end test against a deployed AgentCore runtime**, as for SMA-332.

## 12. Risks

| Risk | Mitigation |
| --- | --- |
| The `helikon.chunk` envelope is a Helikon invention | Narrowed by §7.1 to the events that cannot be content-split; that finite list is documented so a client author knows exactly what needs reassembly. |
| Pacing adds latency | Bounded and constant; the alternative is a connection AgentCore closes outright. Constants are named and justified against §2.2. |
| §2.2's two quota unknowns (window shape, 64 KB units) | Sized against the hostile reading of both; the assumption is written next to the constant. |
| A2A 0.3.0 wire types hand-rolled from the spec could drift from real clients | Contract tests assert exact JSON against AWS's published examples; the `a2a-inspector` tool AWS links is the manual cross-check. Method-name strings are pinned from the spec with a cited source. |
| `TaskStore` is public API on a `0.x` crate | Mirrors `SessionProvider`/`ContextProvider`, already public and stable in shape. Accepted deliberately as the answer to the durability gap. |
| The `axum/ws` declaration is convention, not an enforced invariant (§4.1) | Stated as such rather than presented as guarded. |
| **Scope grew materially after the challenge** — `subscribe`, the cancel registry, CAS, the pacer rewrite, content-level chunking | See §14. A three-way split is now recommended for decision. |

## 13. Acceptance criteria

1. `serve_a2a()` binds `0.0.0.0:9000` and serves `POST /`, `GET /.well-known/agent-card.json`, and
   `GET /ping`, with all five supported JSON-RPC methods, inbound `taskId`/`contextId` handling,
   and the §5.6 error codes.
2. `tasks/resubscribe` attaching mid-run receives backlog and live events with no gap and no
   duplicate; an SSE disconnect leaves the task `working`, not `canceled`.
3. `tasks/cancel` losing the race against a completed run returns `-32002` and leaves the stored
   state `completed`.
4. `serve_agui()` binds `0.0.0.0:8080` and serves `POST /invocations` (AG-UI SSE), `GET /ws`, and
   `GET /ping`; the §6.2 mapping emits `TOOL_CALL_START` before any `TOOL_CALL_ARGS`, produces
   visible text for a delta-free agent, closes every opened pair across all four families, and
   routes unknown variants to `helikon.unknown`.
5. An AG-UI turn-2 request does not double-count history (§6.1).
6. Two sequential requests on one WebSocket connection both complete, and an interrupted run's
   turn is visible to its successor.
7. No outbound WebSocket frame exceeds 60 000 serialized bytes; the pacer's frame count for a
   synthetic sequence is asserted deterministically under a paused clock.
8. `cargo build -p paigasus-helikon-runtime-agentcore --no-default-features` succeeds and is
   asserted in CI.
9. All local CI gates pass: `fmt`, `clippy --workspace --all-features --all-targets -D warnings`,
   `test --workspace --all-features`, `doc` with `RUSTDOCFLAGS=-D warnings`, doc coverage ≥ 80%.
10. README, book page, crate docs, Dockerfile, facade/root READMEs, and examples updated on the
    same branch.

## 14. Recommended decomposition — for decision

The scope grew materially between revision 1 and 2: `TaskStore` gained a subscription primitive
with a lost-wakeup contract, `tasks/cancel` gained a registry and CAS semantics it previously had
no mechanism for at all, `FrameBudget` became a real pacer, and chunking became content-level.
Three-quarters of the AC list is now A2A.

The seam between the three deliverables is cleaner than revision 1 claimed — A2A has no WebSocket
and so shares no `FrameBudget` with AG-UI; the only genuinely shared component is `PingState`,
which already exists:

| PR | Contents | ACs |
| --- | --- | --- |
| 1 | `ws` feature, `FrameBudget`, HTTP-mode `/ws` | 6, 7 |
| 2 | AG-UI: types, mapping, bracketing, SSE, `/ws` | 4, 5 |
| 3 | A2A: types, card, dispatch, `TaskStore`, cancel registry | 1, 2, 3 |

AC8–10 apply to all three. Each is independently shippable with its own README/book delta. **This
is a decision for GATE 1, not a unilateral narrowing** — the ticket's approved scope is all three,
and the default is to proceed as one plan unless the split is chosen.

## 15. Challenge log (revision 1 → 2)

An adversarial review of revision 1 raised 5 BLOCKER, 14 MAJOR, 10 MINOR findings and 6 questions.
Every load-bearing claim was independently verified against this worktree's source before being
accepted (§2.3 records what was checked).

**Folded in — blockers.** Tool-call frames were ordered backwards against the real emission order
(§6.2). `STEP_STARTED` had no closing event, contradicting the stated balance invariant (§6.3).
`tasks/resubscribe` had no live-tail mechanism (§5.5). `tasks/cancel` had no mechanism at all
(§5.7). AG-UI's session model double-counted conversation history on every turn past the first
(§6.1).

**Folded in — majors.** The `#[non_exhaustive]` wildcard arm (§6.2); `MessageOutput`-only agents
producing a blank UI (§6.2); the concurrent-agent interleaving limitation (§6.3); coalescing
mistaken for rate limiting (§7.1); three chunking defects — dropped frames, payload-not-frame
measurement, mid-codepoint splits (§7.1); the interrupt racing the previous finalize (§7.2);
one-shot `CancellationToken` reuse breaking every connection's second message (§7.2);
`TaskStore`'s missing not-found convention, CAS, and per-task event cap (§5.5); inbound
`taskId`/`contextId` (§5.3); AG-UI SSE disconnect semantics (§6.4); the false "server.rs unchanged"
claim (§4.2); the port/route collision guardrail (§6.1); the incorrect `axum/ws` CI rationale
(§4.1); the wall-clock frame-rate test's flake risk (§8); the doc-coverage budget (§9).

**Folded in — minors.** The misleading `0.0.0.0` card URL and the hidden env-var input (§5.2);
manifest completeness and the `jiff` pin note (§4.1); the `Dockerfile` and facade README (§9); the
WebSocket size cap and close code 1003 (§7.2); router return-type asymmetry (§2.3); method-name
pinning (§5.3); the `tools` justification (§6.1); the HTTP-vs-AG-UI `/invocations` warning (§6.1);
the `contextId` echo note (§5.4).

**Answered questions.** Disconnect no longer cancels an A2A task, which was the contradiction with
resubscribe (§5.4). AG-UI `/invocations` is SSE-only (§6.1). Both quota unknowns are stated and
sized conservatively (§2.2). Cross-container task control is out of scope (§11).

**Rejected, with reasons.** *Splitting `ag-ui` into SSE-only plus a separate `/ws` feature* — §2.4
shows tungstenite is unconditionally present regardless, so the split saves nothing real while
adding a third feature to document and test. *Making the new features opt-in* — `mcp` is already
default-on, and making this crate's other three protocol modes opt-in would be a worse
inconsistency than the compile time it saves; the `default-features = false` opt-out is documented
instead. *The three-way PR split* — not rejected, but escalated to GATE 1 (§14) rather than decided
here, since the ticket's approved scope is all three.

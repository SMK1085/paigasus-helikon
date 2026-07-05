# SMA-332 — `paigasus-helikon-runtime-temporal` + `paigasus-helikon-runtime-agentcore` design

**Status:** draft for review (Feature Factory Stage 1/2)
**Ticket:** [SMA-332](https://linear.app/smaschek/issue/SMA-332/paigasus-helikon-runtime-temporal-paigasus-helikon-runtime-agentcore)
**Related:** SMA-392 (session persistence, Done), SMA-422 (terminal-vs-cancel hoist, Backlog), ADR-6 (*Library + pluggable Runner trait*), ADR *Explicit `LoopState` enum*

## 1. Context and goal

Ascend the last two runtime stub crates to real implementations. Both change *where run
state lives* without owning the loop semantics:

- **`paigasus-helikon-runtime-temporal`** — a durable runner: the agent loop becomes a
  Temporal Workflow, each model turn and each `Tool::invoke` becomes a Temporal Activity,
  and `LoopState` + conversation are reconstructed from Temporal history on replay, so a
  crash mid-run resumes from the last completed activity.
- **`paigasus-helikon-runtime-agentcore`** — a managed-deployment shim: an axum server
  that satisfies the AWS Bedrock AgentCore Runtime container contract (HTTP protocol on
  port 8080, MCP protocol on port 8000), plus a multi-stage `Dockerfile` and a CDK
  example.

`paigasus-helikon-core` anticipated this split: `loop_state::transition` is a pure,
IO-free state machine documented as "resumable by construction: a durable runner can
persist `LoopState` plus the accumulated conversation and rehydrate the loop at any
transition boundary". The Temporal crate is the first consumer of that promise.

## 2. Verified facts the design rests on (July 2026 research)

### Temporal Rust SDK

- The official Rust SDK lives in `temporalio/sdk-rust` (renamed from `sdk-core`) and is in
  **supported Public Preview** since Replay 2026 (2026-05-06): "recommended for production
  usage", API may still evolve pre-1.0.
- The full stack is **published on crates.io** since 2026-02-19 under `temporalio-*`
  names: `temporalio-sdk`, `temporalio-client`, `temporalio-sdk-core`, `temporalio-common`,
  `temporalio-macros`, all at **0.5.0** (2026-06-29). The ticket's "backed by
  `temporal-sdk-core` (alpha — pin precisely)" predates this: the old `temporal-sdk-core`
  crates.io name is a stale 2021 placeholder. **A crates.io-published crate can depend on
  the official SDK normally** — the git-dep blocker is gone.
- Licensing: MIT, but declared via `license-file` → crates.io reports "non-standard" →
  `deny.toml` needs `[[licenses.clarify]]` entries for the `temporalio-*` crates.
- TLS: `temporalio-sdk-core`/`temporalio-client` default to `tls-ring`; both expose
  `tls-aws-lc`. We depend with `default-features = false, features = ["tls-aws-lc"]`
  (the gcp_auth dual-CryptoProvider lesson).
- MSRV impact: none (stack MSRV 1.88, edition 2024; workspace is 1.94). New transitive
  weight: tonic 0.14 + prost 0.14 (prost already in our lock; tonic is new).
- SDK shape: `#[workflow]` state struct + `#[workflow_methods]` (`#[init]`, `#[run]`),
  `#[activities]`/`#[activity]` impl blocks, typed `ctx.start_activity(...)`,
  `WorkerOptions::new(queue).register_workflow::<W>().register_activities(A)`.
- Testing: `temporalio-sdk-core`'s `ephemeral-server` feature can download/launch the
  `temporal` CLI dev server programmatically; no SDK-level time-skipping/replay harness
  yet. Env-gated integration tests (à la `sessions-it` / `forkd_live`) are the fit — and
  unlike forkd, a Temporal dev server runs fine on the arm64 macOS dev host.

### AgentCore Runtime contract

- **HTTP protocol** (`serverProtocol: HTTP`): bind `0.0.0.0:8080`; **`POST /invocations`**
  (arbitrary JSON in; `application/json` out, or SSE `text/event-stream` for streaming);
  **`GET /ping`** (required) returning `{"status":"Healthy"}` or `{"status":"HealthyBusy"}`
  (exact casing; `/ping` must never be starved by invocation work — an unresponsive ping
  gets the microVM killed).
- **MCP protocol** (`serverProtocol: MCP`): bind `0.0.0.0:8000`, MCP **streamable-HTTP**
  at `POST /mcp`. Stateless mode is the recommended default; the platform *injects its own
  `Mcp-Session-Id`* — the server must accept unknown session ids without rejecting.
- Sessions: one dedicated microVM per session (`X-Amzn-Bedrock-AgentCore-Runtime-Session-Id`
  header on HTTP, 33–256 chars; `Mcp-Session-Id` on MCP). Idle timeout default 15 min,
  max lifetime 8 h. Termination is abrupt (no documented SIGTERM contract).
- Containers: **linux/arm64 mandatory**, image in private ECR, ≤ 2 GB, ≤ 2 vCPU / 8 GB.
  Platform cold start is ~2–5 s (microVM + pull; indicative, not an SLA) — the "<50 ms"
  AC can only be about *our binary's* init time.
- Limits: 100 MB payload, 15 min sync request, 60 min streaming connection, 10 MB/chunk.
- CDK: stable L2 exists — `aws-cdk-lib/aws_bedrockagentcore` (`Runtime`,
  `AgentRuntimeArtifact.fromEcrRepository`, `addEndpoint`).
- A2A (port 9000) and AG-UI (port 8080) are additional GA protocols — **out of scope**
  here (follow-up tickets if wanted).

## 3. Clarifying questions, answered from context

| Question | Answer | Confidence |
|---|---|---|
| Must both crates ascend to published in this ticket? | Yes — both are implementable against crates.io deps; the standard 4-step ascend applies to each. | High |
| Which Temporal dep pin is "precise" for a published lib? | `temporalio-sdk = "0.5"` etc. (caret on 0.x ⇒ `>=0.5.0, <0.6.0`; 0.6 is the breaking line). An `=0.5.0` pin would poison downstream resolution. | High |
| Does the Temporal runner reuse `LlmAgent`'s driver? | No — it drives `core::transition` itself (that is the ticket's "wraps the existing `LoopState` driver"). `LlmAgent::run`'s extra machinery (hooks, guardrails, redaction, handoff execution) is v0-unsupported in the durable driver (fail fast; see §5.6). | High |
| Is the AgentCore crate a `Runner` implementation? | No — it is a *hosting shim* that delegates to `TokioRunner` inside the container, exactly like `runtime-axum`. The `Runner` seam stays intact. | High |
| Is SMA-422 (terminal-vs-cancel hoist) in scope? | Defer. The Temporal runner maps *workflow status* (Temporal's server resolves the completed-vs-cancelled race authoritatively); the AgentCore shim delegates to `TokioRunner`, which already carries the SMA-421 gate. Neither re-derives the `match` — SMA-422's trigger condition ("2+ implementations shaping the abstraction") still isn't met. | Medium — GATE 1 |
| How is the crash-resume AC demonstrated? | Env-gated integration test against a dev server (`temporalio-sdk-core` `ephemeral-server` feature or external `temporal server start-dev`): kill the worker mid-tool-call, restart, assert the run completes and turn-0 model activity ran exactly once. Loud-skip without the env opt-in, like `forkd_live`. | High |
| How are the AgentCore size/cold-start ACs demonstrated? | Local script + runbook (arm64 macOS builds arm64 images natively): assert image < 30 MB and binary-exec→`/ping`-ready < 50 ms. No new CI gate (Docker-in-CI is a follow-up). | Medium — GATE 1 |

## 4. Approaches considered

### Temporal crate

- **A (chosen): fine-grained durable driver on the official SDK.** Workflow drives
  `core::transition`; one activity per model turn, one activity per tool call. Crash
  mid-tool-call replays the workflow, returns recorded results for completed activities,
  and retries only the in-flight tool. Meets the AC verbatim.
- **B: whole-run-as-one-activity.** Trivial (reuses `LlmAgent::run` wholesale, hooks and
  guardrails included), but a crash re-executes the *entire* run — repaying every model
  call. Fails the AC ("resumes from the last completed activity"). Rejected as the primary
  design; not worth shipping as a secondary mode in v0.
- **C: defer Temporal (Python orchestration tier).** The Notion caveat's fallback from
  May 2026 — obsolete now that the SDK is in supported Public Preview on crates.io.
  Rejected.

### AgentCore crate

- **A (chosen): thin contract shim reusing sibling crates.** Depend on
  `paigasus-helikon-runtime-axum` (`default-features = false`) for `SessionProvider` /
  `ContextProvider` and on `paigasus-helikon-mcp` (behind a crate feature) for the
  streamable-HTTP MCP service. One `AgentCoreServer` builder, two serve modes matching the
  two protocol contracts.
- **B: standalone shim with its own provider traits.** No sibling coupling, but forks
  `SessionProvider`/`ContextProvider` — two diverging trait families for users moving
  between self-hosted and AgentCore deployments. Rejected.
- **C: remount `AgentServer`'s REST router under AgentCore paths.** `AgentServer`'s
  `/agents/{name}/runs` surface doesn't map onto `/invocations`; rewriting routes is a
  hack that inherits DTOs AgentCore callers never see. Rejected.

## 5. Design — `paigasus-helikon-runtime-temporal`

### 5.1 Components

```
crates/paigasus-helikon-runtime-temporal/src/
  lib.rs        — crate docs, re-exports
  payloads.rs   — serde payload types crossing workflow/activity boundaries
  workflow.rs   — AgentLoopWorkflow (#[workflow]): deterministic transition driver
  activities.rs — AgentActivities (#[activities]): call_model + invoke_tool (+ impl detail)
  driver.rs     — DurableDriver: pure, SDK-free step logic (unit-testable)
  worker.rs     — TemporalAgentWorker(Builder): task queue + agent registration + run loop
  runner.rs     — TemporalRunner: implements core::Runner (client side)
  error.rs      — TemporalRunnerError → RunError mapping
```

### 5.2 Split of responsibilities

- **`TemporalRunner` (client side, implements `Runner<Ctx>`):**
  `run()` loads the session snapshot (same `load_and_record` semantics as `TokioRunner`),
  renders the agent's instructions against `RunContext` (instructions render once per run,
  as in `LlmAgent::run`), starts an `AgentLoopWorkflow` with a serialized input payload
  `{agent_name, system_text, conversation, run_config subset}` on the configured task
  queue, awaits the workflow result, maps it to `RunResult`/`RunError`, and appends the
  run's events to the session (best-effort finalize, mirroring `TokioRunner`).
  `ctx.cancel()` → `cancel workflow` request; `RunConfig::timeout` → workflow execution
  timeout. Workflow id: caller-suppliable via config; default `helikon-run-{uuid}`.
- **`AgentLoopWorkflow` (deterministic):** holds `conversation: Vec<Item>`, a *serializable*
  loop position (see 5.3), accumulated `Vec<AgentEvent>`, and drives:
  `transition(...)` → `NextAction::CallModel` ⇒ `start_activity(call_model, request)`;
  `NextAction::ExecuteTools` ⇒ start one `invoke_tool` activity **per call**, concurrently,
  honoring `parallel_tool_call_limit`; `NextAction::Terminate` ⇒ return
  `{final_output | error, events, usage}`. `NextAction::Handoff` ⇒ terminal failure
  ("handoff not supported by the durable runner", see 5.6).
- **`AgentActivities` (worker side, non-deterministic):** built from the registered
  durable agent. `call_model(ModelRequest) -> ModelTurnResult {items, usage, finish_reason}`
  — invokes `Model::invoke`, aggregates the `ModelEvent` stream via the aggregation helper
  hoisted from core (5.5). `invoke_tool(ToolCallRequest) -> ToolCallOutcome` — resolves the
  tool by name, builds a `ToolContext` from the worker's Ctx factory, invokes, stringifies
  errors (matching `ToolCallOutcome::result`'s `Result<Vec<ContentPart>, String>` shape).
  Activity cancellation maps to the tool's `CancellationToken`.
- **`TemporalAgentWorker`:** builder taking `task_queue`, a Temporal client/connection,
  one or more registered agents (5.4), a Ctx factory (`with_ctx`, mirroring
  `McpAgentServer`), and optional activity retry/timeout policies
  (`model_retry_policy`, `tool_retry_policy`, `start_to_close` defaults; sane defaults
  documented). `run()` blocks serving the task queue.

### 5.3 State, serialization, and replay

Temporal's durability model means the workflow does **not** need to serialize `LoopState`
itself for crash-resume — replay re-executes the deterministic workflow code and feeds
recorded activity results back, rebuilding `LoopState` in memory. What must be
serde-serializable are the **payloads**:

- Workflow input: `agent_name`, `system_text`, seeded `conversation: Vec<Item>`,
  driver-scoped `RunConfig` fields (`max_turns`, `parallel_tool_call_limit`; timeouts are
  workflow-level options, `max_agent_depth` irrelevant in v0 — no nesting).
- `call_model` activity: input `ModelRequest`; output `ModelTurnResult { items: Vec<Item>,
  usage: TokenUsage, finish_reason: FinishReason }`.
- `invoke_tool` activity: input `ToolCallRequest`; output `ToolCallOutcome`.
- Workflow result: `DurableRunOutcome { outcome: Ok(FinalOutputPayload) | Err(String…),
  events: Vec<AgentEvent>, usage: TokenUsage }` (structured `AgentError` cannot cross the
  boundary — anyhow isn't serde; the runner reconstructs a typed error class via a small
  serde error-kind enum carried alongside the message).

**Core change (additive):** derive `serde::{Serialize, Deserialize}` on `ModelRequest`,
`ModelSettings`, `ToolDef`, `ToolChoice`, `ResponseFormat`, `FinishReason`,
`ToolCallRequest`, `ToolCallOutcome` (all plain data; `Item`/`ContentPart`/`AgentEvent`/
`TokenUsage` already derive). Where a variant carries non-serde data none do — verified.

Payload-size note: Temporal's default payload cap (~2 MB) and history cap bound very long
conversations; v0 documents this limitation (compaction integration is future work).

### 5.4 What the worker registers (core accessor additions)

`dyn Agent<Ctx>` exposes only `name/description/run`, so the worker cannot extract loop
inputs from an opaque agent. **Core change (additive):** read-only accessors on
`LlmAgent<Ctx, M, T>`:

- `tools(&self) -> &[Arc<dyn Tool<Ctx>>]`
- `model_dyn(&self) -> Arc<dyn Model>` (upcast of the stored `Arc<M>`)
- `model_settings(&self) -> &ModelSettings`
- `output_type(&self) -> Option<&OutputType>`
- `render_instructions(&self, ctx: &RunContext<Ctx>) -> String`
- `has_handoffs / has_hooks / has_input_guardrails / has_output_guardrails (&self) -> bool`
- `default_config(&self) -> &RunConfig`

`TemporalAgentWorker::register(agent: Arc<LlmAgent<Ctx, M, T>>)` snapshots these into an
internal `DurableAgentDef` keyed by agent name. Registration **fails fast** if the agent
has hooks, guardrails, or handoffs configured (v0 constraint, 5.6). The client cannot
pre-validate that the worker knows `agent.name()` (client and worker may be different
processes); an unregistered name fails at the first activity, and the runner maps that
activity failure to a descriptive `RunError`.

### 5.5 Model-turn aggregation helper (core change, additive)

`LlmAgent`'s private `build_items` logic (ModelEvent stream → `Vec<Item>` + usage +
finish reason, including tool-call delta accumulation) moves to a small public core
helper (working name `core::aggregate_model_turn(stream) -> Result<ModelTurn, ModelError>`),
and `LlmAgent` consumes it internally — single source of truth, no drift between the
ephemeral and durable drivers. Raw `TokenDelta`/`ReasoningDelta` fidelity inside an
activity is not re-emitted to the caller (see 5.7).

### 5.6 v0 constraint set (explicit, fail-fast)

Unsupported in the durable driver v0 — all documented in the crate docs and rejected at
**registration time** with a descriptive error (never silently ignored):

- Handoffs (`NextAction::Handoff` additionally guards at runtime → terminal failure).
- Hooks and guardrails (they are arbitrary user async code; running them deterministically
  in-workflow is impossible, and running them in activities is a design decision deferred
  past v0).
- Nested agents (agent-as-tool executes inside `invoke_tool` opaquely — that works, but
  the nested run is not itself durable; documented).
- `Compacting` / `NeedsApproval` loop states (not driveable in core yet either).

### 5.7 Streaming semantics

`run()` is fully supported. `run_streamed()` v0: starts the workflow, awaits completion,
then yields the recorded `AgentEvent`s as an immediate stream followed by the terminal —
**documented as "buffered, not live"** (satisfies the trait contract and lets
`collect()`-based call sites work unchanged). Live streaming (workflow
queries/update-with-progress) is future work. Raw token deltas never cross the activity
boundary; the durable event log contains semantic events only (`TurnStarted`,
`MessageOutput`, `ToolCallItem`, `ToolOutputItem`, terminals).

### 5.8 Error, cancellation, retry mapping

- Workflow completed with `Failed` loop state → `RunError::Agent` reconstructed from the
  serde error-kind (e.g. `MaxTurnsExceeded`, `InvalidStructuredOutput` carry their data;
  others degrade to `AgentError::Other(message)`).
- Workflow cancelled → `RunError::Cancelled`; workflow execution timeout →
  `RunError::Timeout`. (Temporal's server decides the completed-vs-cancelled race — no
  local precedence resolver needed; SMA-422 stays deferred.)
- Client/connection errors → `RunError::Other`.
- Activity retries: **follow Temporal's convention — activities retry by default**
  (server-default retry policy with exponential backoff), for both `call_model` and
  `invoke_tool`. A worker crash mid-activity surfaces as an attempt timeout and the next
  attempt re-dispatches on a live worker, so the crash-resume AC holds with defaults.
  Consequence (documented prominently): tool idempotency is the tool author's
  responsibility, exactly as for any Temporal activity; non-idempotent tools set
  `tool_retry_policy` to `max_attempts: 1` via the worker builder, accepting that a crash
  mid-call then fails the run instead of resuming it. Both policies (`model_retry_policy`,
  `tool_retry_policy`) plus start-to-close defaults are builder-configurable.

### 5.9 Testing

- **Unit (no server):** payload serde round-trips; `DurableDriver` step logic (transition
  sequencing, parallel-tool fan-out ordering, handoff rejection, error-kind mapping) —
  the driver is deliberately SDK-free so this needs no Temporal.
- **Integration (env-gated, loud-skip):** `tests/temporal_live.rs`, gated on
  `TEMPORAL_TEST_SERVER` (URL of a running dev server) or `TEMPORAL_TEST_SERVER=ephemeral`
  (use `temporalio-sdk-core`'s `ephemeral-server` to download+launch the CLI dev server;
  dev-dependency only). Tests: (1) happy-path multi-turn tool run with a `MockModel`
  (scripted per-request responses — worker-side, no network); (2) **crash-resume AC**:
  tool blocks on first invocation → abort the worker task mid-tool-call → start a fresh
  worker on the same task queue → run completes; assert the model activity for turn 0
  executed exactly once (counter in the activities impl) — proving resume from history,
  not re-execution; (3) cancellation maps to `RunError::Cancelled`; (4) session round-trip:
  second `run` on the same session sees the first turn's messages.
- Validated locally on the dev host (temporal CLI runs on arm64 macOS). No new CI job in
  this PR; a `temporal-it` CI job is proposed as a follow-up ticket.

## 6. Design — `paigasus-helikon-runtime-agentcore`

### 6.1 Components

```
crates/paigasus-helikon-runtime-agentcore/src/
  lib.rs      — crate docs, re-exports
  server.rs   — AgentCoreServer(Builder): HTTP-protocol serve on 0.0.0.0:8080
  invoke.rs   — POST /invocations handler: payload DTO, SSE + buffered JSON responses
  ping.rs     — GET /ping handler + PingStatus (Healthy/HealthyBusy) shared state
  session.rs  — session-id header extraction/validation (33–256 chars), provider glue
  mcp.rs      — (feature `mcp`) serve_mcp on 0.0.0.0:8000 mounting streamable-HTTP /mcp
  error.rs    — contract-shaped error responses
examples/
  echo_http.rs        — dependency-free echo agent (HTTP protocol; the size-AC binary)
  agent_http.rs       — model-backed example (feature-gated on a provider, e.g. anthropic)
  mcp_server.rs       — MCP-protocol variant
docker/Dockerfile     — multi-stage arm64 build (see 6.4)
```

### 6.2 HTTP protocol mode

`AgentCoreServer::builder().agent(...).runner(default TokioRunner).session_provider(...)
.context_provider(...).run_config(...).build()?.serve().await` binds `0.0.0.0:8080`:

- **`POST /invocations`** — body: `RunRequest`-shaped JSON (reuse
  `runtime-axum`'s `RunRequest` DTO: `{input: string | messages}`), tolerant of the
  AgentCore convention `{"prompt": "..."}` (accepted alias). Session resolved from
  `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` (validated 33–256 chars; absent header ⇒
  ephemeral in-memory session, since one microVM == one session anyway). Response:
  default **SSE** (`text/event-stream`, one `data: <AgentEvent JSON>` frame per event,
  eager flush) via `run_streamed`; `Accept: application/json` ⇒ buffered
  `{final_output, usage}` via `run`.
- **`GET /ping`** — always-responsive dedicated handler returning
  `{"status":"Healthy"}` / `{"status":"HealthyBusy"}` from an `Arc<PingState>`; v0 never
  sets `HealthyBusy` itself (no background jobs) but the state + a public setter ship so
  agent tools can flag long async work; `time_of_last_update` set only on genuine
  transitions (per AWS guidance).
- Depends on `paigasus-helikon-runtime-axum { default-features = false }` for
  `SessionProvider`/`InMemorySessionProvider`, `ContextProvider`/`DefaultContextProvider`,
  and the `RunRequest` DTO — one provider vocabulary across self-hosted and AgentCore
  deployments.

### 6.3 MCP protocol mode (crate feature `mcp`, default on)

`AgentCoreServer::serve_mcp()` binds `0.0.0.0:8000` and mounts
`McpAgentServer::streamable_http_service()` at `/mcp` in **stateless mode** — required so
the platform-injected, never-initialized `Mcp-Session-Id` is accepted.
**`paigasus-helikon-mcp` change (additive):** `streamable_http_service()` currently
hardcodes `StreamableHttpServerConfig::default()`; add a config knob (e.g.
`streamable_http_service_with(config)` or a `stateless()` builder toggle) exposing rmcp's
stateless mode. A trivial `/ping` also ships on 8000 (not contractually required for MCP;
cheap insurance).

### 6.4 Dockerfile, size and cold-start ACs, CDK

- `docker/Dockerfile`: multi-stage — `rust:1.94-alpine` (musl) builder compiling
  `--release --example echo_http` for `aarch64-unknown-linux-musl`, stripped, into
  `FROM scratch` with only the static binary. Target: **image < 30 MB** (expected
  ~5–10 MB for the echo example — no TLS stack). The model-backed example documents the
  `distroless/cc` variant (aws-lc-rs/rustls need more care under musl; that image may
  exceed 30 MB and is not the AC binary).
- Cold start: the binary logs `ready in {ms}` after the listener binds;
  `scripts/agentcore-image-check.sh` builds the image, asserts size < 30 MB, runs the
  container, and asserts exec→`/ping`-200 < 50 ms (measured app-side; AWS's own microVM
  provisioning of ~2–5 s is outside the contract we can influence — reframed AC, see §8).
- CDK: verified `aws-cdk-lib/aws_bedrockagentcore` L2 snippet (Runtime +
  `AgentRuntimeArtifact.fromEcrRepository` + `addEndpoint`) goes in the crate README and
  the book page; MCP variant notes `protocolConfiguration: ProtocolType.MCP` (exact enum
  member to be confirmed against the CDK version at implementation time) and the port
  contract.
- Abrupt-termination note in docs: no SIGTERM guarantee — durable state belongs in the
  `Session` backend, not container memory (pairs naturally with `sessions-postgres`/
  `-redis` for cross-session persistence; in-memory is fine within one microVM session).

### 6.5 Testing

- `Router::oneshot` unit tests: `/ping` shape (exact casing), `/invocations` JSON mode,
  `/invocations` SSE mode (frame framing + terminal event), session header validation
  (too-short id rejected with contract-shaped 400), `{"prompt": ...}` alias acceptance.
- MCP mode: in-process rmcp client against the stateless service with a *pre-set unknown*
  `Mcp-Session-Id` header (the platform-injection scenario).
- Docker build + size + cold-start script run locally (arm64 host); results recorded in
  the PR. A CI docker-build job is a follow-up, not part of this PR.

## 7. Release engineering

One PR on `feature/sma-332-…` carrying, per the CLAUDE.md rituals:

1. **Ascend `runtime-temporal`**: `0.0.0` → `0.1.0`, drop `publish = false`, drop its
   `release-plz.toml` block.
2. **Ascend `runtime-agentcore`**: same 4 steps.
3. **Core bump (5th step)**: serde derives + `LlmAgent` accessors + `aggregate_model_turn`
   are same-PR core API consumed by an ascending crate ⇒ bump `paigasus-helikon-core`
   (patch), its `[workspace.dependencies]` pin, CHANGELOG.
4. **`paigasus-helikon-mcp` bump**: the stateless-config addition is same-PR API consumed
   by ascending `runtime-agentcore` ⇒ manual patch bump + pin + CHANGELOG (same
   cargo-verify deadlock logic as core).
5. **Facade bump (6th step)**: manual sibling bumps defeat `dependencies_update` ⇒ patch-
   bump `paigasus-helikon` + self-pin + CHANGELOG so the published facade requests the
   real `runtime-temporal`/`runtime-agentcore` versions. Facade `lib.rs` re-export docs
   for both features checked (`missing_docs` gate).
6. `[workspace.dependencies]`: add `temporalio-sdk`/`-client`/`-sdk-core` (0.5,
   `default-features = false` + `tls-aws-lc` where applicable) and the internal version
   bumps for the two ascending crates.
7. `deny.toml`: `[[licenses.clarify]]` MIT entries for the `temporalio-*` crates (plus any
   new transitive license surprises cargo-deny finds).
8. Docs in the same PR: both crate READMEs (real content + `cargo add` + contract tables +
   CDK snippet), facade README feature table, root README roster, book pages
   (`introduction.md` stub roster line, installation/features page, a runtimes concept
   page section per crate), CHANGELOGs. `mdbook build docs/book` clean.
9. New runbook: `docs/runbooks/agentcore-image-check.md` (+ script); Temporal local
   validation instructions live in the crate README (dev-server one-liner).

Versioning note: `runtime-axum` gains no API change (`default-features = false` reuse
only) ⇒ no manual bump (already-released consumer rule). If implementation ends up
touching its API after all, it joins the manual-bump list.

## 8. Acceptance-criteria mapping

| Ticket AC | Status in this design |
|---|---|
| "Temporal: a run that crashes mid-tool-call resumes from the last completed activity" | Met verbatim — integration test (5.9 #2) kills the worker mid-tool-call and asserts completion without re-running completed activities. |
| "AgentCore: container builds to <30 MB" | Met by the echo-example scratch/musl image, asserted by `scripts/agentcore-image-check.sh`. The model-backed variant is documented but not size-gated. |
| "AgentCore: cold-starts in <50 ms" | **Reinterpreted** (GATE 1): AWS's microVM provisioning is ~2–5 s and not ours to control; the AC is applied to the app-side share — binary exec → `/ping` ready < 50 ms, asserted by the same script. |

## 9. Out of scope / follow-up candidates

- A2A (port 9000, GA) and AG-UI protocol shims — new tickets if wanted.
- Live streaming from the Temporal workflow (queries/updates) — v0 is buffered.
- Hooks/guardrails/handoffs in the durable driver; incremental per-transition session
  persistence from inside the workflow (SMA-392's "durable runners may persist
  incrementally" note) — the v0 runner finalizes client-side like `TokioRunner`.
- `temporal-it` CI job (dev-server-in-CI); Docker-build CI job.
- SMA-422 hoist — stays in Backlog; trigger condition still unmet (no third stream-wrapping
  runner emerged).
- WebSocket `/ws` endpoint on the AgentCore HTTP protocol (optional per contract).

## 10. Open questions for GATE 1

1. **Single PR for both crates** (recommended; independent module trees, one release
   train) — or split into two sequential PRs ("Part of SMA-332" + "Closes SMA-332")?
2. **`run_streamed` v0 = buffered-after-completion** (recommended) — or a hard
   "unsupported" error until live streaming exists?
3. **Fail-fast registration** when a durable agent has hooks/guardrails/handoffs
   (recommended) — or accept-and-ignore with a warning?
4. **Cold-start AC reinterpretation** (§8) acceptable?
5. **No new CI gates in this PR** (temporal tests env-gated; docker checks scripted +
   runbook) acceptable?
6. `deny.toml` license clarifications for `temporalio-*` (MIT via `license-file`) — any
   concern with the growing clarify list?

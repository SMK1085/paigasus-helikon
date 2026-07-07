# SMA-332 — `paigasus-helikon-runtime-temporal` + `paigasus-helikon-runtime-agentcore` design

**Status:** revised after adversarial challenge (Feature Factory Stage 2) — awaiting GATE 1 approval
**Ticket:** [SMA-332](https://linear.app/smaschek/issue/SMA-332/paigasus-helikon-runtime-temporal-paigasus-helikon-runtime-agentcore)
**Related:** SMA-392 (session persistence, Done), SMA-422 (terminal-vs-cancel hoist, Backlog), ADR-6 (*Library + pluggable Runner trait*), ADR-10 (*retries are an application-layer concern*), ADR *Explicit `LoopState` enum*

## 1. Context and goal

Ascend the last two runtime stub crates to real implementations. Both change *where run
state lives* without owning the loop semantics:

- **`paigasus-helikon-runtime-temporal`** — a durable runner: the agent loop becomes a
  Temporal Workflow, each model turn and each `Tool::invoke` becomes a Temporal Activity,
  and the loop position is reconstructed via deterministic replay of Temporal history, so
  a crash mid-run resumes from the last completed activity.
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
- **Implementation-time checkpoints** (challenge findings): (a) confirm whether
  `temporalio-protos`/`temporalio-sdk-core` need a system `protoc` at build time — if so,
  every CI job that compiles the workspace (and contributors' machines) needs it, which is
  a rollout cost to surface before merging, not after; (b) confirm the transitive TLS
  backend of the `ephemeral-server` feature before enabling it anywhere the
  `--all-features` gate compiles (see §5.10).
- SDK shape: `#[workflow]` state struct + `#[workflow_methods]` (`#[init]`, `#[run]`),
  `#[activities]`/`#[activity]` impl blocks, typed `ctx.start_activity(...)`,
  `WorkerOptions::new(queue).register_workflow::<W>().register_activities(A)`.

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
| Does the Temporal runner reuse `LlmAgent`'s driver? | No — it drives `core::transition` itself (that is the ticket's "wraps the existing `LoopState` driver"), with the between-transition responsibilities spelled out in §5.3. `LlmAgent::run`'s hook/guardrail/handoff machinery is v0-unsupported (fail fast; §5.7). | High |
| Is the AgentCore crate a `Runner` implementation? | No — it is a *hosting shim* that delegates to `TokioRunner` inside the container, exactly like `runtime-axum`. The `Runner` seam stays intact. | High |
| Is SMA-422 (terminal-vs-cancel hoist) in scope? | Defer. The Temporal runner maps *workflow outcomes* (the workflow itself resolves the completed-vs-cancelled race — §5.9); the AgentCore shim delegates to `TokioRunner`, which already carries the SMA-421 gate. Neither re-derives the `match` — SMA-422's trigger condition ("2+ implementations shaping the abstraction") still isn't met. | Medium — GATE 1 |
| How is the crash-resume AC demonstrated? | Env-gated integration test against a dev server: kill the worker mid-tool-call, restart, assert the run completes and turn-0 model activity ran exactly once. Loud-skip without the env opt-in, like `forkd_live`. | High |
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
  two protocol contracts. (The request DTO is crate-own, not reused — see §6.2.)
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
  workflow.rs   — AgentLoopWorkflow (#[workflow]): thin adapter over DurableDriver
  activities.rs — AgentActivities (#[activities]): call_model + invoke_tool
  driver.rs     — DurableDriver: pure, SDK-free step logic (unit-testable)
  worker.rs     — TemporalAgentWorker(Builder): task queue + agent registration + run loop
  runner.rs     — TemporalRunner: implements core::Runner (client side)
  error.rs      — error-kind payload enum + TemporalRunnerError → RunError mapping
```

### 5.2 Split of responsibilities

- **`TemporalRunner` (client side, implements `Runner<Ctx>`):**
  `run()` loads the session snapshot (same `load_and_record` semantics as `TokioRunner`)
  [as built: `run()` itself never renders instructions — `Runner::run` has no instructions
  access. Rendering happens inside the workflow, as the first activity (`render_instructions`),
  against the *worker*-fabricated `RunContext` (§5.8), not the caller's], starts an
  `AgentLoopWorkflow` with a serialized input payload
  `{agent_name, system_text, conversation, driver config, deadline}` on the
  configured task queue, awaits the workflow result, maps it to `RunResult`/`RunError`,
  and appends the run's events to the session. **Finalize runs on every exit path**: the
  workflow returns a `DurableRunOutcome` for completed, agent-failed, cancelled, *and*
  timed-out runs (§5.9), so partial events are persisted for all four; only
  infrastructure-level failures (workflow unreachable/terminated/payload errors) can yield
  no events, and then the runner still appends the recorded new-turn input before
  surfacing `RunError` (matching `TokioRunner`'s best-effort-write posture).
  `ctx.cancel()` → workflow cancellation request. Workflow id: caller-suppliable via
  config; default `helikon-run-{uuid}` (uuid generated client-side — never inside the
  workflow).
- **`AgentLoopWorkflow` (deterministic, thin):** delegates every decision to
  `DurableDriver` (§5.3) and performs only the effects the driver requests:
  `start_activity(call_model, …)`, one `invoke_tool` activity **per call** started
  concurrently (bounded by `parallel_tool_call_limit`), durable timer for the run
  deadline, and cooperative-cancellation handling (§5.9).
- **`AgentActivities` (worker side, non-deterministic):** built from the registered
  durable agent (§5.5). `call_model(ModelRequest) -> ModelTurnResult` — invokes
  `Model::invoke`, drains the event stream through the shared `ModelTurnAccumulator`
  (§5.6). `invoke_tool(ToolCallRequest) -> ToolCallOutcome` — executes the **full
  tool-call pipeline** hoisted from core (§5.4): authorize → invoke → redact → content
  conversion, against the worker-fabricated `RunContext` (§5.8). Tool-level errors are
  **data, not activity failures**: they return as `ToolCallOutcome.result = Err(String)`
  exactly as the ephemeral loop feeds tool errors back to the model. Only
  infrastructure-level panics/crashes fail the activity attempt.
- **`TemporalAgentWorker`:** builder taking `task_queue`, a Temporal client/connection,
  one or more registered agents, a worker-context factory (§5.8), and optional activity
  policies (`model_retry_policy`, `tool_retry_policy`, start-to-close/heartbeat defaults —
  §5.9). `run()` blocks serving the task queue.

### 5.3 The `DurableDriver` contract (mirrors `LlmAgent`'s driver exactly)

The driver is a pure struct (no Temporal types) so its sequencing is unit-testable and its
behavior is defined by construction, not by implementer interpretation. Its contract,
mirroring `agent.rs`:

1. **Seed:** `conversation = [Item::System{system_text}]` (omitted when empty) `++
   seeded messages`; emit `AgentEvent::RunStarted { agent }` (the ephemeral driver emits
   it outside `transition`; the durable driver replicates it for event parity).
2. **Step:** call `transition(state, input, ctx)` with `TransitionCtx { tools, settings,
   max_turns, conversation, output, handoffs: &[] }`.
3. **After `CallModel` completes:** append the returned `items` to `conversation`, then
   feed `TransitionInput::ModelResponse`.
4. **After `ExecuteTools` completes:** append one `Item::ToolResult` per outcome to
   `conversation` **in the original call order** (deterministic reassembly independent of
   activity completion order), then feed `TransitionInput::ToolResults` with outcomes in
   that same order.
5. **Every step:** append `TransitionOutcome::conversation_appends` (e.g. the structured-
   output repair message) to `conversation` before the next step, and accumulate
   `TransitionOutcome::events`.
6. **Terminate:** `Done` → success outcome; `Failed(err)` → serialize the error kind
   (§5.9); `NextAction::Handoff` → terminal failure ("handoff not supported by the
   durable runner v0").

The driver exposes `next_effect()`/`apply(result)` so the workflow is a mechanical
effect-executor; unit tests drive it with scripted results and assert conversation
mutation, event ordering, and fan-out ordering with zero Temporal machinery.

### 5.4 Core change: hoist the per-call tool-execution pipeline (additive)

In the ephemeral path, permission enforcement (`authorize`), output **redaction**
(`ToolContext.redact_output` defaults to `true`), and `ToolOutput → Vec<ContentPart>`
conversion live in `LlmAgent`'s private `run_tools_concurrent` — **not** in
`Tool::invoke`. A durable activity calling `Tool::invoke` directly would silently skip
authorization and write **unredacted** tool output into Temporal history (a permanent
external record). Core therefore exposes the single-call pipeline as a public helper
(working name `core::execute_tool_call(tool, &ToolContext<Ctx>, ToolCallRequest) ->
ToolCallOutcome`), and both `run_tools_concurrent` and the Temporal activity consume it —
one pipeline, no drift. Redaction and permission checks apply to the activity **result
before Temporal records it**.

### 5.5 What the worker registers

`LlmAgent`'s fields are already `pub` (deliberately, per its docs) — **no new accessors
are added to core** (challenge finding: they'd be redundant API surface).
`TemporalAgentWorker::register(agent: Arc<LlmAgent<Ctx, M, T>>)` snapshots `tools`,
`model` (upcast `Arc<M>` → `Arc<dyn Model>` at the generic registration site — no core
change needed), `model_settings`, `output_type`, and the instructions provider into an
internal `DurableAgentDef` keyed by agent name. Registration **fails fast** if the agent
has hooks, guardrails, or handoffs configured (v0 constraint, §5.7). The client cannot
pre-validate that the worker knows `agent.name()` (client and worker may be different
processes); an unregistered name fails at the first activity, and the runner maps that to
a descriptive `RunError`.

### 5.6 Core change: shared model-turn accumulation (additive)

`LlmAgent`'s stream loop both **yields live deltas** (`TokenDelta`/`ReasoningDelta`/
`ToolCallDelta`) and accumulates state that its private `build_items` reassembles into
items. The durable activity must not lose that reassembly logic, and the ephemeral driver
must not lose live streaming. Core therefore exposes the accumulation as a small state
machine (working name `core::ModelTurnAccumulator`: `observe(&ModelEvent)`,
`finish() -> Result<ModelTurn { items, usage, finish_reason }, …>` with the invalid-tool-
args case folded into its error type), built by refactoring the existing private logic.
`LlmAgent` keeps its inline loop — observing each event into the accumulator *and*
yielding deltas live; the activity drains without yielding. Single source of truth for
reassembly; no streaming regression.

### 5.7 v0 constraint set (explicit, fail-fast)

Unsupported in the durable driver v0 — all documented in the crate docs and rejected at
**registration time** with a descriptive error (never silently ignored):

- Handoffs (`NextAction::Handoff` additionally guards at runtime → terminal failure).
- Hooks and guardrails (arbitrary user async code; running them deterministically
  in-workflow is impossible, and running them in activities is a design decision deferred
  past v0).
- Nested agents (agent-as-tool executes inside `invoke_tool` opaquely — that works, but
  the nested run is not itself durable; documented).
- `Compacting` / `NeedsApproval` loop states (not driveable in core yet either).

Permissions and redaction are **not** in this list — they are enforced worker-side via
the hoisted pipeline (§5.4) with the worker context's posture (§5.8).

### 5.8 Ctx and security posture at the client→worker boundary

`Ctx` is not serializable in general, so the caller's request-scoped context — tenant
data, auth claims, permission rules, approval handler, tracer — **does not cross to the
worker**. The worker fabricates the tool-side `RunContext<Ctx>` from its own configured
factory (mirroring `McpAgentServer::with_ctx`) plus an optional worker-side permission/
redaction configuration; that worker posture is authoritative for every tool call it
executes, with the same safe defaults as core (`redact_output = true`, permission rules
as configured on the worker context). This is documented loudly in the crate docs:
**worker-side posture, not caller posture, governs durable tool execution**, and
Temporal history should be treated as a persistence boundary (redaction applies before
recording).

**As-built (landed in SMA-455).** v0 shipped this section's posture as fixed and
non-configurable, and named the serializable-`Ctx`-seed mechanism as future work. Both
landed in SMA-455: the optional worker-side posture configuration described above is now
`worker::WorkerPosture<Ctx>`, set via `TemporalAgentWorkerBuilder::posture(...)`
(`WorkerPosture::default()` reproduces the v0 fixed defaults exactly); the serializable
seed is `runner::TemporalRunnerConfig::with_ctx_seed(Value)` on the client side and
`TemporalAgentWorkerBuilder::with_seeded_ctx` / `::try_with_seeded_ctx` on the worker
side. See
`docs/superpowers/specs/2026-07-06-runtime-temporal-worker-posture-design.md` for the
full design (including the fail-fast contract on a malformed seed and the seed→policy
composition that gives per-run authorization without ever serializing the policy).

[As built (SMA-455): configurable via `WorkerPosture` +
`with_ctx_seed`/`with_seeded_ctx`/`try_with_seeded_ctx`; see the as-built note above.]

### 5.9 Outcome, error, cancellation, timeout, retry semantics

- **Workflow result is total:** `DurableRunOutcome { status, events, usage }` where
  `status ∈ {Completed(FinalOutputPayload), AgentFailed(ErrorKindPayload), Cancelled,
  TimedOut}`. `ErrorKindPayload` is a small serde enum carrying data for the typed cases
  (`MaxTurnsExceeded(u32)`, `InvalidStructuredOutput{schema_errors, final_text}`,
  `Model{message}` [as built: message only, no `kind`] …) and degrading to a message
  otherwise; the runner reconstructs
  `RunError::Agent(...)` / `RunError::Cancelled` / `RunError::Timeout` from it, so events
  are returned (and finalized into the session) on **every** driver-level exit path —
  matching `TokioRunner`'s finalize-on-every-exit guarantee.
- **Timeout:** `RunConfig::timeout` becomes a **durable timer raced inside the workflow**
  (deterministic), not a workflow-execution timeout — on expiry the workflow returns
  `TimedOut` *with the events so far*. A hard workflow-execution timeout (which would
  discard the outcome) is set only as a backstop margin above the timer.
- **Cancellation:** the runner's cancel request is handled **cooperatively** in the
  workflow: it stops driving, returns `Cancelled` with events-so-far. Terminal-wins
  precedence (the `Runner` docs' contract) holds structurally: a workflow that already
  reached `Done`/`Failed` has returned — a late cancel finds nothing to override.
- **Activity failure → typed errors:** `call_model` converts every `ModelError` into a
  **non-retryable** application error carrying `ErrorKindPayload` — per **ADR-10**
  ("the runner never retries [model errors] — retries are an application-layer concern");
  users who want model retries wrap the model in `runtime-tokio`'s `RetryingModel` on the
  worker, the existing application-layer mechanism. The workflow converts the activity
  failure into `AgentFailed` (events preserved). Tool-level errors never fail the activity
  at all (§5.2).
- **Crash-resume vs error-retry are distinct by construction:** retry policies allow
  re-dispatch (so a worker crash/timeout mid-activity resumes on a live worker — the AC),
  while genuine application errors are marked non-retryable and terminate immediately.
  Consequence (documented prominently): a crash mid-`invoke_tool` re-executes that tool
  call — tool idempotency under crash-retry is the tool author's responsibility, exactly
  as for any Temporal activity; tools that must never re-execute set
  `tool_retry_policy.max_attempts = 1`, accepting that a crash then fails the run instead
  of resuming it.

### 5.10 Determinism and upgrade discipline (operational guidance)

The workflow's deterministic core is `core::transition` — which lives in a separately
evolving crate. Replay of an in-flight workflow against a worker carrying a *different*
core produces Temporal non-determinism errors. v0 ships with documented operational
guidance rather than machinery: (a) agent runs are minutes-to-hours, not months — **drain
in-flight runs before redeploying workers with a bumped core/temporal crate** (blue-green
task queues make this trivial: new deployment serves a new queue name); (b) the crate's
CHANGELOG flags any release whose transition behavior changed as replay-breaking;
(c) Temporal Worker Versioning (Build IDs) is noted as the production-grade path once the
Rust SDK's support matures. The `ephemeral-server` test feature is used **only** if its
transitive TLS backend is verified aws-lc-clean (§2 checkpoint); otherwise integration
tests gate exclusively on an externally launched `temporal server start-dev`
(`TEMPORAL_TEST_SERVER=<url>`), keeping the `--all-features` CI gate free of a second
rustls provider.

### 5.11 Payload and history budget (quantified)

`transition` builds every `ModelRequest` from the **full** conversation, so each
`call_model` input carries the whole conversation-so-far: history cost grows
quadratically with turns, and Temporal's default gRPC payload cap (~2–4 MB, server
`blob-size` warnings ~512 KB) bounds a single payload. Practical v0 envelope
(documented in the crate docs with the arithmetic): conversations must stay under
~1.5 MB of JSON — roughly a 15–20-turn run with tool outputs averaging ≤ 50 KB; a single
tool result larger than ~1.5 MB fails its activity outright. v0 explicitly targets runs
inside that envelope; the documented escape hatches are bounding tool output size and
`max_turns`. Payload codecs / claim-check blob offloading and compaction integration are
named follow-up work, not silent gaps.

### 5.12 Streaming semantics

`run()` is fully supported. `run_streamed()` v0: starts the workflow, awaits completion,
then yields the recorded `AgentEvent`s as an immediate stream followed by the terminal —
**documented as "buffered, not live"**, and explicitly noting the inversion of the trait
docs' persistence contract: the trait warns that dropping the stream may skip
persistence; here persistence happened *before* the stream exists (a strictly stronger
guarantee), so dropping the buffered stream loses nothing. `RunResultStreaming::
with_failure` is wired so `collect()` surfaces the typed error, matching `TokioRunner`.
Live streaming (workflow queries/updates) is future work. Raw token deltas never cross
the activity boundary; the durable event log contains semantic events only.

### 5.13 Testing

- **Unit (no server):** payload serde round-trips; `DurableDriver` contract tests
  (conversation mutation per §5.3, call-order reassembly under out-of-order completion,
  repair-message append, handoff rejection, RunStarted parity, error-kind mapping,
  timeout/cancel partial outcomes) — the driver is SDK-free so this needs no Temporal.
- **Integration (env-gated, loud-skip):** `tests/temporal_live.rs`, gated on
  `TEMPORAL_TEST_SERVER=<url>` (externally launched dev server; `ephemeral` mode only if
  the §2 TLS checkpoint passes). Tests: (1) happy-path multi-turn tool run with a
  scripted `MockModel` (worker-side, no network); (2) **crash-resume AC**: tool blocks on
  first invocation (short start-to-close ~5 s + heartbeat so the attempt times out
  promptly) → abort the worker task mid-tool-call → start a fresh worker on the same task
  queue → run completes; assert the turn-0 `call_model` executed exactly once (counter in
  the activities impl) — resume from history, not re-execution; (3) cancellation returns
  `RunError::Cancelled` *and* the session contains the partial transcript; (4) session
  round-trip: a second `run` on the same session sees the first turn's messages.
- Validated locally on the dev host (temporal CLI runs on arm64 macOS). No new CI job in
  this PR; a `temporal-it` CI job is proposed as a follow-up ticket.

## 6. Design — `paigasus-helikon-runtime-agentcore`

### 6.1 Components

```
crates/paigasus-helikon-runtime-agentcore/src/
  lib.rs      — crate docs, re-exports
  server.rs   — AgentCoreServer(Builder): HTTP-protocol serve on 0.0.0.0:8080
  invoke.rs   — POST /invocations handler: InvocationRequest DTO, SSE + JSON responses
  ping.rs     — GET /ping handler + PingStatus (Healthy/HealthyBusy) shared state
  session.rs  — session-id header extraction/validation (33–256 chars), provider glue
  mcp.rs      — (feature `mcp`) serve_mcp on 0.0.0.0:8000 mounting streamable-HTTP /mcp
  error.rs    — contract-shaped error responses
examples/
  echo_http.rs        — dependency-free echo agent (HTTP protocol; minimal-overhead image)
  agent_http.rs       — model-backed example (feature-gated on a provider; the size-AC image, §6.4)
  mcp_server.rs       — MCP-protocol variant
docker/Dockerfile     — multi-stage arm64 build (see 6.4)
```

### 6.2 HTTP protocol mode

`AgentCoreServer::builder().agent(...).runner(default TokioRunner).session_provider(...)
.context_provider(...).run_config(...).build()?.serve().await` binds `0.0.0.0:8080`:

- **`POST /invocations`** — body: a crate-own `InvocationRequest` DTO accepting
  `{"prompt": "..."}` (the common AgentCore convention), `{"input": "..."}`, or
  `{"messages": [...]}`. (Challenge finding: `runtime-axum`'s `RunRequest` deserializes
  with `deny_unknown_fields` and cannot accept `prompt` — the DTO is therefore not
  reused; the session/context **providers** still are.) Session resolved from
  `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` (validated 33–256 chars; absent header ⇒
  ephemeral in-memory session, since one microVM == one session anyway). Response:
  default **SSE** (`text/event-stream`, one `data: <AgentEvent JSON>` frame per event,
  eager flush) via `run_streamed`; `Accept: application/json` ⇒ buffered
  `{final_output, usage}` via `run`. The SSE frames use the same wire shape as
  `runtime-axum`'s SSE endpoint (serde `AgentEvent`, `#[non_exhaustive]`): consumers must
  tolerate unknown variants; this is documented as the crate's wire contract, and a
  versioned envelope is named as future work rather than invented ad hoc here.
- **`GET /ping`** — always-responsive dedicated handler returning
  `{"status":"Healthy"}` / `{"status":"HealthyBusy"}` from an `Arc<PingState>`; v0 never
  sets `HealthyBusy` itself (no background jobs) but the state + a public setter ship so
  agent tools can flag long async work; `time_of_last_update` set only on genuine
  transitions (per AWS guidance).
- Depends on `paigasus-helikon-runtime-axum { default-features = false }` for
  `SessionProvider`/`InMemorySessionProvider` and `ContextProvider`/
  `DefaultContextProvider` — one provider vocabulary across self-hosted and AgentCore
  deployments.

### 6.3 MCP protocol mode (crate feature `mcp`, default on)

`AgentCoreServer::serve_mcp()` binds `0.0.0.0:8000` and mounts the agent's MCP
streamable-HTTP service at `/mcp` in **stateless mode** — required so the
platform-injected, never-initialized `Mcp-Session-Id` is accepted.
**`paigasus-helikon-mcp` change (additive):** `streamable_http_service()` currently
hardcodes `StreamableHttpServerConfig::default()`; add a config knob (e.g.
`streamable_http_service_with(config)` or a `stateless()` builder toggle). Implementation
checkpoint: verify rmcp 1.7's stateless mode accepts arbitrary pre-set session ids; if
its config can't express that, the fallback is a thin session-manager shim in the
agentcore crate that does. Session-backend note (challenge finding): `McpAgentServer`
currently builds a fresh `MemorySession` per tool call, so **MCP-mode AgentCore cannot
use a persistent session backend in v0** — documented; the "durable state belongs in the
`Session` backend" guidance applies to HTTP mode only. A trivial `/ping` also ships on
8000 (not contractually required for MCP; cheap insurance).

### 6.4 Dockerfile, size and cold-start ACs, CDK

- `docker/Dockerfile`: multi-stage — `rust:1.94-alpine` (musl) builder compiling the
  example for `aarch64-unknown-linux-musl`, stripped, into `FROM scratch`.
- **Size AC applies to the model-backed image** (challenge finding: gating only a
  dependency-free echo binary would demonstrate nothing about a deployable agent). The
  primary target: `agent_http` (a real provider client + rustls/aws-lc-rs, statically
  linked under musl) in a scratch image **< 30 MB** — expected feasible (static binary
  ~15–25 MB stripped) but not guaranteed; if aws-lc-rs-under-musl proves intractable
  within the ticket, the recorded fallback is: echo image < 30 MB (framework overhead
  proof) + the model-backed image's real size documented in the README — **explicitly a
  GATE 1 decision, not a silent downgrade**. The echo image ships either way as the
  minimal-overhead demonstration (~5–10 MB).
- Cold start: the binary logs `ready in {ms}` after the listener binds;
  `scripts/agentcore-image-check.sh` builds both images, asserts the size gate, runs the
  container, and asserts exec→`/ping`-200 < 50 ms (measured app-side; AWS's own microVM
  provisioning of ~2–5 s is outside the contract we can influence — reframed AC, §8).
- CDK: verified `aws-cdk-lib/aws_bedrockagentcore` L2 snippet (Runtime +
  `AgentRuntimeArtifact.fromEcrRepository` + `addEndpoint`) goes in the crate README and
  the book page; MCP variant notes `protocolConfiguration: ProtocolType.MCP` (exact enum
  member confirmed against the CDK version at implementation time) and the port contract.
- Abrupt-termination note in docs: no SIGTERM guarantee — durable state belongs in the
  `Session` backend (HTTP mode; see §6.3 for the MCP caveat), not container memory.

### 6.5 Testing

- `Router::oneshot` unit tests: `/ping` shape (exact casing), `/invocations` JSON mode,
  `/invocations` SSE mode (frame framing + terminal event), session header validation
  (too-short id rejected with contract-shaped 400), all three `InvocationRequest` body
  forms.
- MCP mode: in-process rmcp client against the stateless service with a *pre-set unknown*
  `Mcp-Session-Id` header (the platform-injection scenario).
- Docker build + size + cold-start script run locally (arm64 host); results recorded in
  the PR. A CI docker-build job is a follow-up, not part of this PR.

## 7. Release engineering

One PR on `feature/sma-332-…` carrying, per the CLAUDE.md rituals:

1. **Ascend `runtime-temporal`**: `0.0.0` → `0.1.0`, drop `publish = false`, drop its
   `release-plz.toml` block.
2. **Ascend `runtime-agentcore`**: same 4 steps.
3. **Core bump (5th step)**: serde derives (§5.3's payload types: `ModelRequest`,
   `ModelSettings`, `ToolDef`, `ToolChoice`, `ResponseFormat`, `FinishReason`,
   `ToolCallRequest`, `ToolCallOutcome` — all plain data; `Item`/`ContentPart`/
   `AgentEvent`/`TokenUsage` already derive) + `execute_tool_call` (§5.4) +
   `ModelTurnAccumulator` (§5.6) are same-PR core API consumed by an ascending crate ⇒
   bump `paigasus-helikon-core` (patch), its `[workspace.dependencies]` pin, CHANGELOG.
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
8. **Lint/doc obligations for both new crates** (challenge finding): `[lints]
   workspace = true` opt-in blocks, `///` on every public item (`missing_docs` +
   `-D warnings` docs job), and the 80% doc-coverage gate absorbing the new public
   surface.
9. Docs in the same PR: both crate READMEs (real content + `cargo add` + contract tables +
   CDK snippet), facade README feature table, root README roster, book pages
   (`introduction.md` stub roster line, installation/features page, a runtimes concept
   page section per crate), CHANGELOGs. `mdbook build docs/book` clean.
10. New runbook: `docs/runbooks/agentcore-image-check.md` (+ script); Temporal local
    validation instructions live in the crate README (dev-server one-liner).
11. **Build-tooling checkpoint before the first commit that adds the Temporal dep**: if
    `temporalio-*` needs system `protoc`, surface it immediately — it would touch every
    CI job and contributor setup, and may argue for revisiting the dependency shape.

Versioning note: `runtime-axum` gains no API change (`default-features = false` reuse
only) ⇒ no manual bump (already-released consumer rule). If implementation ends up
touching its API after all, it joins the manual-bump list.

## 8. Acceptance-criteria mapping

| Ticket AC | Status in this design |
|---|---|
| "Temporal: a run that crashes mid-tool-call resumes from the last completed activity" | Met with default policies — retries re-dispatch crashed attempts while application errors are non-retryable (§5.9); integration test (§5.13 #2) kills the worker mid-tool-call and asserts completion without re-running completed activities. |
| "AgentCore: container builds to <30 MB" | Primary target: the **model-backed** image < 30 MB (scratch/musl); recorded fallback if aws-lc-rs/musl blocks: echo image gated + real image size documented — GATE 1 decision (§6.4). |
| "AgentCore: cold-starts in <50 ms" | **Reinterpreted** (GATE 1): AWS's microVM provisioning is ~2–5 s and not ours to control; the AC is applied to the app-side share — binary exec → `/ping` ready < 50 ms, asserted by `scripts/agentcore-image-check.sh`. |

## 9. Out of scope / follow-up candidates

- A2A (port 9000, GA) and AG-UI protocol shims — new tickets if wanted.
- Live streaming from the Temporal workflow (queries/updates) — v0 is buffered.
- Hooks/guardrails/handoffs in the durable driver; incremental per-transition session
  persistence from inside the workflow (SMA-392's "durable runners may persist
  incrementally" note) — v0 finalizes client-side from the total `DurableRunOutcome`.
  (Serializable-`Ctx` seed propagation, listed here at v0 time, landed in SMA-455 — see
  §5.8's as-built note.)
- Temporal payload codec / claim-check blob offloading; compaction integration (§5.11).
- Temporal Worker Versioning (Build IDs) integration (§5.10).
- `temporal-it` CI job (dev-server-in-CI); Docker-build CI job.
- SMA-422 hoist — stays in Backlog; trigger condition still unmet (no third stream-wrapping
  runner emerged).
- WebSocket `/ws` endpoint on the AgentCore HTTP protocol (optional per contract).
- Versioned SSE event envelope for the AgentCore wire format (shared concern with
  `runtime-axum`).

## 10. Open questions for GATE 1

1. **Single PR for both crates** (recommended; independent module trees, one release
   train) — or split into two sequential PRs ("Part of SMA-332" + "Closes SMA-332")?
2. **`run_streamed` v0 = buffered-after-completion** (recommended) — or a hard
   "unsupported" error until live streaming exists?
3. **Fail-fast registration** when a durable agent has hooks/guardrails/handoffs
   (recommended) — or accept-and-ignore with a warning?
4. **Cold-start AC reinterpretation** (§8) acceptable?
5. **Size-AC target**: commit to the model-backed image < 30 MB with the documented
   fallback path (§6.4) — acceptable framing?
6. **No new CI gates in this PR** (temporal tests env-gated; docker checks scripted +
   runbook) acceptable?
7. `deny.toml` license clarifications for `temporalio-*` (MIT via `license-file`) — any
   concern with the growing clarify list?
8. **ADR-10 interaction** (§5.9): model errors non-retryable at the Temporal layer,
   `RetryingModel` as the sanctioned retry mechanism — confirm this reading of ADR-10 for
   the durable runner.

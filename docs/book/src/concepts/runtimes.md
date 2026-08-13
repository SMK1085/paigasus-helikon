# Runtimes

`paigasus-helikon-core`'s `Runner` trait is the seam between the agent-loop state machine (`core::transition`, driven inline by `LlmAgent::run`) and *where a run actually executes*. Five crates implement or host that seam today, each trading off differently on durability, deployment shape, and operational complexity:

| Crate | Feature | What it is | When to use it |
| --- | --- | --- | --- |
| [`paigasus-helikon-runtime-tokio`](../reference/crates.md) | `runtime-tokio` | Default ephemeral in-process runner | A single process, no crash-resume needs, no network exposure required |
| [`paigasus-helikon-runtime-axum`](../reference/crates.md) | `runtime-axum` | Self-hosted HTTP/SSE/WebSocket agent server | You need a network-accessible agent server with replayable runs, and you own the deployment |
| [`paigasus-helikon-runtime-actix`](../reference/crates.md) | `runtime-actix` | Self-hosted actix-web HTTP/SSE/WebSocket agent server, API-identical to `runtime-axum` | You're embedding into an existing actix-web service |
| `paigasus-helikon-runtime-temporal` | `runtime-temporal` | Durable runner backed by the [Temporal](https://temporal.io) Rust SDK | A run must survive a worker crash or restart — long-running, multi-tool-call agents where losing progress is expensive |
| `paigasus-helikon-runtime-agentcore` | `runtime-agentcore` | AWS Bedrock AgentCore container shim | You want a managed, serverless-ish deployment target on AWS and are fine with AgentCore's container/microVM contract |

The first three are covered in their own chapters — see [Axum Server Runtime](./axum-server.md) for the self-hosted HTTP/SSE/WebSocket server and its actix-web variant (`paigasus-helikon-runtime-actix`). This page covers the other two, both first shipped in the same release (SMA-332); the AgentCore section below also covers its A2A and AG-UI protocol modes, added in SMA-461.

## `paigasus-helikon-runtime-temporal` — durable runner

`TemporalRunner` implements `Runner`, but instead of driving `core::transition` in-process, it starts a Temporal *workflow* and awaits its result. Each model turn and each tool call becomes a Temporal *activity*; Temporal's event-history replay reconstructs the loop's position after a worker crash, so **a run that crashes mid-tool-call resumes from the last completed activity** rather than re-executing the whole run.

### Architecture in one paragraph

The client-side `TemporalRunner::run()` starts an internal agent-loop workflow on a configured task queue and awaits its outcome. The workflow is a thin, deterministic adapter over a pure `DurableDriver` (SDK-free, unit-testable) that drives `core::transition` exactly like the ephemeral loop does, but requests a `call_model` activity per model turn and one `invoke_tool` activity per tool call (started concurrently, bounded by `parallel_tool_call_limit`) instead of calling `Model::invoke`/`Tool::invoke` directly. A `TemporalAgentWorker` process registers the agent's tools/model/settings and serves those activities.

### v0 constraint set (rejected at registration, fail-fast)

- **Hooks and guardrails** — arbitrary user async code cannot be made deterministic inside a workflow.
- **Handoffs** — agent-to-agent transfers are not supported by the durable driver.
- **Nested agent-as-tool runs** — permitted, but execute *non-durably* inside their tool activity: a crash-retry re-executes the whole nested run.

### Worker-side security posture

The caller's `RunContext` (tenant data, auth claims, permission rules) does **not** cross the client→worker boundary — it isn't serializable in general. The worker fabricates its own tool-side context from its configuration, and *that* configuration — not the caller's — is authoritative for permission enforcement and output redaction on every tool call the worker executes. Treat Temporal's durable history as a persistence boundary: redaction applies before a tool result is recorded, exactly as it does in the ephemeral loop.

That worker-side posture is **configurable**, not fixed: a `WorkerPosture` value set via `TemporalAgentWorkerBuilder::posture(...)` groups the permission mode, deny/allow/guard rules, a permission policy, an approval handler, the built-in destructive guards, output redaction, and extra secrets into one builder call (`WorkerPosture::default()` reproduces the original fixed defaults exactly, so an unconfigured worker behaves as before). A client can also attach a small, explicit, serializable seed (`TemporalRunnerConfig::with_ctx_seed`) that the worker's seeded `Ctx` factory reconstitutes per run — request-scoped caller data, never posture — enabling a worker-registered permission policy to make per-tenant authorization decisions without ever serializing the policy itself (when the posture mode reaches the policy — `Bypass` and `DontAsk` short-circuit it). See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Worker-Side Posture and Security Boundary") for the full contract, including the fail-fast-on-bad-seed guarantee.

### Retry, heartbeats, and payload notes

Model errors are non-retryable at the Temporal layer (per ADR-10 — wrap the model in `runtime-tokio`'s `RetryingModel` for retries); tool errors are returned as data and never fail the activity; a crash mid-tool-call is retried by Temporal, so tools must be idempotent unless configured with `max_attempts: 1`. Opt-in heartbeats (`TemporalAgentWorkerBuilder::heartbeat_interval`) speed up reclaiming a *crashed* worker's in-flight `call_model`/`invoke_tool` attempt, but can also trip on a *live* worker whose executor is starved by a blocking tool call — offload blocking work via `tokio::task::spawn_blocking`. Each activity payload carries the full conversation-so-far (plus the `Ctx` seed, if set, on every `render_instructions`/`invoke_tool` call), so plan for roughly 1.5 MB of JSON as the practical v0 budget (~15–20 turns with tool outputs ≤ 50 KB each). Those payloads are a single self-describing envelope per activity, so future additions to an activity's *own* input fields stay backward- and forward-compatible — a change inside a nested `paigasus-helikon-core` type such as `ModelRequest` still breaks the wire. Upgrading a worker fleet also wants care: see the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Upgrade Discipline and Determinism") for the one-release-at-a-time and drain-before-rollback rules.

See the [crate README](https://docs.rs/paigasus-helikon-runtime-temporal) and its crate-level docs for the full contract, worked examples, and live-validation instructions (`temporal server start-dev` + an env-gated integration suite covering crash-resume).

## `paigasus-helikon-runtime-agentcore` — managed AWS deployment

`AgentCoreServer` doesn't implement `Runner` itself — like `runtime-axum`, it's a *hosting shim*: an axum app that wraps a `Runner` (`TokioRunner` by default) behind the exact HTTP contract AWS Bedrock AgentCore's Runtime expects, so the same agent can be deployed as a managed AgentCore container without hand-rolling the platform's endpoints.

AgentCore recognizes four container protocols; this crate implements all four. A container is configured for exactly one `serverProtocol`, so a deployment calls exactly one `serve_*` method:

| Protocol | Bind | Endpoint(s) | Contract highlight |
| --- | --- | --- | --- |
| HTTP (default) | `0.0.0.0:8080` | `POST /invocations` (JSON or SSE), `GET /ping`, optional `GET /ws` (feature `ws`) | `/ping` returns exactly `{"status":"Healthy"}` / `{"status":"HealthyBusy"}`; session pinning via `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` (33–256 chars) |
| MCP (feature `mcp`, default on) | `0.0.0.0:8000` | `POST /mcp` (stateless streamable-HTTP) | Platform-injected `Mcp-Session-Id` must be accepted without ever having been issued by this server |
| A2A (feature `a2a`, default on) | `0.0.0.0:9000` | `POST /` (JSON-RPC 2.0), `GET /.well-known/agent-card.json`, `GET /ping` | Errors are **A2A-specification** codes on an HTTP `200`; AWS's `-32051`…`-32055` table is platform-side and never emitted from the container |
| AG-UI (feature `ag-ui`, default on) | `0.0.0.0:8080` | `POST /invocations` (SSE), `GET /ws`, `GET /ping` | Shares HTTP mode's port *and* path — merge one router or the other into an app, never both |

**A2A tasks and durability.** A2A adds a task lifecycle (`submitted → working → completed/failed/canceled`) addressable by `tasks/get` and re-attachable by `tasks/resubscribe`. Those only mean something across container lifetimes if tasks outlive one, and the default `InMemoryTaskStore` does not — implement the `TaskStore` trait over a database and install it with `AgentCoreServerBuilder::task_store` when clients rely on resubscription. Note also that a **client disconnect does not cancel an A2A task**, deliberately inverting `/invocations`' behaviour: resubscription exists to survive a dropped stream, so only `tasks/cancel` produces `canceled`.

**AG-UI is stateless per request.** AG-UI clients resend the whole conversation each turn while the runner seeds the model with `history ++ input.messages`, so pairing a persisted session with a full client history would double-count every prior turn. Each request therefore gets a fresh, unshared session — AG-UI mode cannot use a persistent session backend in v0, the same limitation MCP mode carries. AG-UI's event vocabulary also assumes one active text/tool-call span at a time, so an agent interleaving two tool calls has its spans serialized onto the wire rather than genuinely nested.

**WebSocket frame quotas.** Both WebSocket endpoints pace and split outbound frames to stay inside AgentCore's documented 64 KB/frame and 250 frames/second limits, budgeting against the conservative reading of each. Splitting keeps a frame a valid protocol event wherever it can — a long text delta becomes several smaller deltas — falling back to `helikon.chunk` envelopes only for events that cannot be split into several valid events.

Containers are **linux/arm64 mandatory**, ship from a private ECR repository, and are capped at 2 GB / 2 vCPU / 8 GB. This crate's `docker/Dockerfile` builds a `scratch`/musl image; measured on the ticket's validation run, the dependency-free `echo_http` example built to 1.31 MB and the model-backed `agent_http` example (real provider client, rustls/aws-lc-rs, statically linked) to 3.27 MB — both comfortably under the 30 MB size gate — with app-exec-to-`/ping`-ready cold starts of 9–11 ms, well under the 50 ms acceptance-criteria gate (CI re-measures on a shared arm64 runner against a deliberately looser 250 ms budget — a difference of measuring instrument, not a relaxed criterion). (AWS's own microVM provisioning, documented at roughly 2–5 seconds, happens platform-side before the container's entrypoint ever runs, and is outside what this measurement — or this crate — can influence.) See [`docs/runbooks/agentcore-image-check.md`](https://github.com/SMK1085/paigasus-helikon/blob/main/docs/runbooks/agentcore-image-check.md) for the full measurement procedure.

AWS publishes a stable CDK L2 construct for deployment: `aws-cdk-lib/aws-bedrockagentcore`'s `Runtime` construct takes an `AgentRuntimeArtifact.fromEcrRepository(...)` and exposes `.addEndpoint(...)`; the same `Runtime` accepts `protocolConfiguration: ProtocolType.MCP`, `ProtocolType.A2A`, or `ProtocolType.AG_UI` to switch it out of the default HTTP mode. The full snippet lives in the [crate README](https://docs.rs/paigasus-helikon-runtime-agentcore).

**Client disconnects (HTTP and AG-UI modes)**: both `/invocations` transports — buffered JSON and SSE — drive the run on a detached task and hold a `CancellationToken` drop-guard, so a client that walks away mid-run cancels the run *and* still gets its turn finalized into the `Session`. The persisted turn is partial (whatever the run had produced when it was cancelled), which is the deliberate trade: a disconnected client should not keep burning model tokens. Note that agentcore has no per-session serialization lock (unlike [`runtime-axum`](./axum-server.md)), so a cancelled run's finalize can overlap a retry of the same session id — AgentCore pins a session to a container, which makes genuinely concurrent same-session invocations unlikely enough in practice that the lock isn't worth its cost here.

**Termination is abrupt** — AgentCore documents no `SIGTERM` contract. In HTTP mode, durable conversation state belongs in a `Session` backend (SQLite/Postgres/Redis), never in container memory. In MCP and AG-UI modes this doesn't apply the same way: each call gets a fresh, unshared in-memory session by construction, so neither can use a persistent session backend in v0 at all — a known v0 limitation, not a deployment mistake to fix via configuration. In A2A mode it applies twice over: conversation state belongs in a `Session` backend *and* tasks belong in a durable `TaskStore`, or both vanish with the microVM.

## Choosing between them

- Need a network endpoint you host and operate yourself, with no crash-resume requirement? **`runtime-axum`** — or **`runtime-actix`** if you're embedding into an existing actix-web service; the two are API-identical apart from a handful of unavoidable framework deltas (see [Axum Server Runtime](./axum-server.md#actix-web-variant)).
- Need a run to survive a worker crash or a long-running multi-hour agent to keep progress across restarts? **`runtime-temporal`.**
- Deploying onto AWS Bedrock AgentCore's managed Runtime platform specifically? **`runtime-agentcore`** — note its HTTP mode still delegates to `TokioRunner` under the hood (no crash-resume of its own; pair with `runtime-temporal`'s `Runner` if both properties are needed simultaneously, though that combination is untested as of this writing).
- Everything else (tests, single-shot scripts, embedding in your own async runtime)? **`runtime-tokio`**, the default.

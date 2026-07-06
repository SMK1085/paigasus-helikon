# Runtimes

`paigasus-helikon-core`'s `Runner` trait is the seam between the agent-loop state machine (`core::transition`, driven inline by `LlmAgent::run`) and *where a run actually executes*. Four crates implement or host that seam today, each trading off differently on durability, deployment shape, and operational complexity:

| Crate | Feature | What it is | When to use it |
| --- | --- | --- | --- |
| [`paigasus-helikon-runtime-tokio`](../reference/crates.md) | `runtime-tokio` | Default ephemeral in-process runner | A single process, no crash-resume needs, no network exposure required |
| [`paigasus-helikon-runtime-axum`](../reference/crates.md) | `runtime-axum` | Self-hosted HTTP/SSE/WebSocket agent server | You need a network-accessible agent server with replayable runs, and you own the deployment |
| `paigasus-helikon-runtime-temporal` | `runtime-temporal` | Durable runner backed by the [Temporal](https://temporal.io) Rust SDK | A run must survive a worker crash or restart — long-running, multi-tool-call agents where losing progress is expensive |
| `paigasus-helikon-runtime-agentcore` | `runtime-agentcore` | AWS Bedrock AgentCore container shim | You want a managed, serverless-ish deployment target on AWS and are fine with AgentCore's container/microVM contract |

The first two are covered in their own chapters — see [Axum Server Runtime](./axum-server.md) for the self-hosted HTTP/SSE/WebSocket server. This page covers the other two, both shipped in the same release (SMA-332).

## `paigasus-helikon-runtime-temporal` — durable runner

`TemporalRunner` implements `Runner`, but instead of driving `core::transition` in-process, it starts a Temporal *workflow* and awaits its result. Each model turn and each tool call becomes a Temporal *activity*; Temporal's event-history replay reconstructs the loop's position after a worker crash, so **a run that crashes mid-tool-call resumes from the last completed activity** rather than re-executing the whole run.

### Architecture in one paragraph

The client-side `TemporalRunner::run()` starts an `AgentLoopWorkflow` on a configured task queue and awaits its outcome. The workflow is a thin, deterministic adapter over a pure `DurableDriver` (SDK-free, unit-testable) that drives `core::transition` exactly like the ephemeral loop does, but requests a `call_model` activity per model turn and one `invoke_tool` activity per tool call (started concurrently, bounded by `parallel_tool_call_limit`) instead of calling `Model::invoke`/`Tool::invoke` directly. A `TemporalAgentWorker` process registers the agent's tools/model/settings and serves those activities.

### v0 constraint set (rejected at registration, fail-fast)

- **Hooks and guardrails** — arbitrary user async code cannot be made deterministic inside a workflow.
- **Handoffs** — agent-to-agent transfers are not supported by the durable driver.
- **Nested agent-as-tool runs** — permitted, but execute *non-durably* inside their tool activity: a crash-retry re-executes the whole nested run.

### Worker-side security posture

The caller's `RunContext` (tenant data, auth claims, permission rules) does **not** cross the client→worker boundary — it isn't serializable in general. The worker fabricates its own tool-side context from its configuration, and *that* configuration — not the caller's — is authoritative for permission enforcement and output redaction on every tool call the worker executes. Treat Temporal's durable history as a persistence boundary: redaction applies before a tool result is recorded, exactly as it does in the ephemeral loop.

### Retry and payload notes

Model errors are non-retryable at the Temporal layer (per ADR-10 — wrap the model in `runtime-tokio`'s `RetryingModel` for retries); tool errors are returned as data and never fail the activity; a crash mid-tool-call is retried by Temporal, so tools must be idempotent unless configured with `max_attempts: 1`. Each activity payload carries the full conversation-so-far, so plan for roughly 1.5 MB of JSON as the practical v0 budget (~15–20 turns with tool outputs ≤ 50 KB each).

See the [crate README](https://docs.rs/paigasus-helikon-runtime-temporal) and its crate-level docs for the full contract, worked examples, and live-validation instructions (`temporal server start-dev` + an env-gated integration suite covering crash-resume).

## `paigasus-helikon-runtime-agentcore` — managed AWS deployment

`AgentCoreServer` doesn't implement `Runner` itself — like `runtime-axum`, it's a *hosting shim*: an axum app that wraps a `Runner` (`TokioRunner` by default) behind the exact HTTP contract AWS Bedrock AgentCore's Runtime expects, so the same agent can be deployed as a managed AgentCore container without hand-rolling the platform's endpoints.

AgentCore recognizes two container protocols; this crate implements both:

| Protocol | Bind | Endpoint(s) | Contract highlight |
| --- | --- | --- | --- |
| HTTP (default) | `0.0.0.0:8080` | `POST /invocations` (JSON or SSE), `GET /ping` | `/ping` returns exactly `{"status":"Healthy"}` / `{"status":"HealthyBusy"}`; session pinning via `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` (33–256 chars) |
| MCP (feature `mcp`, default on) | `0.0.0.0:8000` | `POST /mcp` (stateless streamable-HTTP) | Platform-injected `Mcp-Session-Id` must be accepted without ever having been issued by this server |

Containers are **linux/arm64 mandatory**, ship from a private ECR repository, and are capped at 2 GB / 2 vCPU / 8 GB. This crate's `docker/Dockerfile` builds a `scratch`/musl image; measured on the ticket's validation run, the dependency-free `echo_http` example built to 1.31 MB and the model-backed `agent_http` example (real provider client, rustls/aws-lc-rs, statically linked) to 3.27 MB — both comfortably under the 30 MB size gate — with app-exec-to-`/ping`-ready cold starts of 9–11 ms, well under the 50 ms gate. (AWS's own microVM provisioning, documented at roughly 2–5 seconds, happens platform-side before the container's entrypoint ever runs, and is outside what this measurement — or this crate — can influence.) See [`docs/runbooks/agentcore-image-check.md`](https://github.com/SMK1085/paigasus-helikon/blob/main/docs/runbooks/agentcore-image-check.md) for the full measurement procedure.

AWS publishes a stable CDK L2 construct for deployment: `aws-cdk-lib/aws-bedrockagentcore`'s `Runtime` construct takes an `AgentRuntimeArtifact.fromEcrRepository(...)` and exposes `.addEndpoint(...)`; the same `Runtime` accepts `protocolConfiguration: ProtocolType.MCP` to switch it into MCP mode. The full snippet lives in the [crate README](https://docs.rs/paigasus-helikon-runtime-agentcore).

**Termination is abrupt** — AgentCore documents no `SIGTERM` contract. In HTTP mode, durable conversation state belongs in a `Session` backend (SQLite/Postgres/Redis), never in container memory. In MCP mode this doesn't apply the same way: each MCP call gets a fresh, unshared in-memory session by construction, so MCP-mode AgentCore cannot use a persistent session backend in v0 at all — that's a known v0 limitation, not a deployment mistake to fix via configuration.

## Choosing between them

- Need a network endpoint you host and operate yourself, with no crash-resume requirement? **`runtime-axum`.**
- Need a run to survive a worker crash or a long-running multi-hour agent to keep progress across restarts? **`runtime-temporal`.**
- Deploying onto AWS Bedrock AgentCore's managed Runtime platform specifically? **`runtime-agentcore`** — note its HTTP mode still delegates to `TokioRunner` under the hood (no crash-resume of its own; pair with `runtime-temporal`'s `Runner` if both properties are needed simultaneously, though that combination is untested as of this writing).
- Everything else (tests, single-shot scripts, embedding in your own async runtime)? **`runtime-tokio`**, the default.

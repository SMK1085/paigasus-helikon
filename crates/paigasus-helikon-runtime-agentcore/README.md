# paigasus-helikon-runtime-agentcore

AWS Bedrock AgentCore runtime shim for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `AgentCoreServer` wraps a `paigasus-helikon-core` [`Agent`](https://docs.rs/paigasus-helikon-core/latest/paigasus_helikon_core/trait.Agent.html) in an [axum](https://crates.io/crates/axum) app that satisfies AWS Bedrock AgentCore's Runtime container contract — its default **HTTP protocol** (port 8080, with an optional WebSocket), its **MCP protocol** (port 8000), its **A2A protocol** (port 9000), or its **AG-UI protocol** (port 8080) — so the same agent can be deployed as a managed AgentCore Runtime without hand-rolling the contract's endpoints. It delegates execution to [`paigasus-helikon-runtime-tokio`](https://crates.io/crates/paigasus-helikon-runtime-tokio)'s `TokioRunner` by default and reuses `paigasus-helikon-runtime-axum`'s `SessionProvider`/`ContextProvider` traits, so a self-hosted deployment and an AgentCore deployment of the same agent share one provider vocabulary.

## Install

```bash
cargo add paigasus-helikon-runtime-agentcore
```

Most users enable the `runtime-agentcore` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::runtime_agentcore`:

```bash
cargo add paigasus-helikon --features runtime-agentcore
```

## Quick start

The crate ships a dependency-free `echo_http` example — no model provider, no TLS stack — as the fastest way to see the HTTP-protocol contract end to end:

```bash
cargo run -p paigasus-helikon-runtime-agentcore --example echo_http

curl -s localhost:8080/ping
curl -s -X POST localhost:8080/invocations \
    -H 'content-type: application/json' -H 'accept: application/json' \
    -d '{"prompt":"hi there"}'
```

Building your own server looks like:

```rust,ignore
use std::sync::Arc;

use paigasus_helikon_runtime_agentcore::AgentCoreServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = AgentCoreServer::<()>::builder()
        .with_default_context() // Ctx = () satisfies Default
        .agent(Arc::new(my_agent))
        .build()?;

    server.serve().await?; // binds 0.0.0.0:8080, blocks until terminated
    Ok(())
}
```

A model-backed variant (behind the `example-anthropic` feature, used as the size/cold-start acceptance-criteria image) ships as `examples/agent_http.rs`.

## HTTP-protocol mode (`AgentCoreServer::serve`)

Binds `0.0.0.0:8080` — AgentCore's fixed HTTP-protocol port — and never returns until the process is terminated.

### `GET /ping`

A dedicated, always-responsive health-check handler backed by its own shared state — it never touches the runner, the agent, the session provider, or any in-flight invocation, so a stuck or slow invocation can never delay or starve a health check.

| Status | JSON body | When |
| --- | --- | --- |
| `200 OK` | `{"status":"Healthy"}` | Steady state (default) |
| `200 OK` | `{"status":"HealthyBusy"}` | After `PingState::set_busy(true)` (exposed via `AgentCoreServer::ping_state()` for tools flagging long-running async work) |

Both variants add `time_of_last_update` (Unix seconds) once a genuine status *transition* has occurred — never on the initial steady state, and never re-stamped by a repeated call reporting the same status. The exact casing (`Healthy` / `HealthyBusy`) is part of AgentCore's contract and is not configurable.

### `POST /invocations`

Request body — exactly one of three JSON shapes:

| Shape | Example | Semantics |
| --- | --- | --- |
| `messages` | `{"messages": [...]}` | Explicit `Item` list — multi-turn context or non-text content parts |
| `prompt` | `{"prompt": "hi there"}` | Shorthand for a single user text message |
| `input` | `{"input": "hi there"}` | Identical semantics to `prompt`; AgentCore's own SDK examples use both spellings |

Response — selected by the request's `Accept` header:

| `Accept` | Status | Body |
| --- | --- | --- |
| `application/json` | `200 OK` | Buffered `{"final_output": "...", "usage": {...}}`, returned once the run reaches a terminal event |
| default / `text/event-stream` | `200 OK` | Server-Sent Events — one `data: <AgentEvent JSON>` frame per event, terminated by the run's `RunCompleted`/`RunFailed` event |

Session resolution — an optional request header pins the invocation to a session:

| Header | Validation | Absent |
| --- | --- | --- |
| `X-Amzn-Bedrock-AgentCore-Runtime-Session-Id` | 33–256 characters (rejected with `400` otherwise) | Fresh, unshared session (one microVM instance is, by AgentCore's execution model, already one session) |

### `GET /ws` (feature `ws`, default on)

An optional endpoint of the HTTP-protocol contract, carrying the same request vocabulary as `POST /invocations` over a persistent connection.

| | |
| --- | --- |
| Inbound | One **text** frame per request, each a JSON `InvocationRequest` (the same three body shapes) |
| Outbound | One JSON text frame per `AgentEvent`, terminated by `run_completed`/`run_failed` |
| Binary frames | Unsupported — the connection closes with code `1003` |
| Malformed JSON | A `run_failed` frame; the connection stays open |
| Concurrency | One run at a time. A request arriving mid-run cancels the in-flight run and **waits for it to finish** before starting the next, so the interrupted turn's session write lands first |
| Session | Read once from the upgrade request's headers, validated exactly as `/invocations` does |

## A2A-protocol mode (feature `a2a`, default on; `AgentCoreServer::serve_a2a`)

AgentCore's **A2A runtime type**: JSON-RPC 2.0 on port 9000 with agent-card discovery.

| | |
| --- | --- |
| Bind address | `0.0.0.0:9000` (fixed, distinct from HTTP/AG-UI's 8080 and MCP's 8000) |
| Endpoints | `POST /` (JSON-RPC 2.0), `GET /.well-known/agent-card.json`, `GET /ping` |

| Method | Transport | Behaviour |
| --- | --- | --- |
| `message/send` | JSON | Runs to completion; returns the finished `Task` with its artifacts |
| `message/stream` | SSE | Same run, streamed as `status-update` / `artifact-update` events |
| `tasks/get` | JSON | Store lookup |
| `tasks/cancel` | JSON | Cancels a live run — see the taxonomy below |
| `tasks/resubscribe` | SSE | Replays a task's event log, then live-tails it to the terminal |
| `tasks/pushNotificationConfig/*` | JSON error | `-32003` PushNotificationNotSupported |
| authenticated extended card | JSON error | `-32004` UnsupportedOperation |
| anything else | JSON error | `-32601` MethodNotFound |

**Error codes are A2A-specification codes carried on an HTTP `200`, never AWS's platform table.** AWS documents `-32051`…`-32055` for conditions its *platform* reports to a client in front of the container (throttling, runtime unavailable). Those are never emitted from inside it. What this crate emits is the specification's taxonomy — `-32001` TaskNotFound, `-32002` TaskNotCancelable, `-32005` ContentTypeNotSupported, plus the JSON-RPC core codes.

Inbound `taskId` / `contextId` on `message/send` and `message/stream`:

| Inbound | Behaviour |
| --- | --- |
| `taskId` absent | Mint a new task (the common case) |
| `taskId` names a known, non-terminal task | Continue it |
| `taskId` names a known, terminal task | `-32602` — a terminal task cannot take more input |
| `taskId` names an unknown task | `-32001` |
| `contextId` present, session header absent | Use it as the context |
| `contextId` disagrees with the session header | **Header wins** — platform-authoritative beats client-supplied |

Request parts are text-only; a `file` or `data` part answers `-32005` rather than being silently dropped.

`tasks/cancel` answers `-32001` for an unknown task, `-32002` for one already terminal, `-32002` for one with no live run in this container, and `-32002` when the run reaches its own terminal first — in that last case the stored state is left exactly as the run wrote it, never overwritten with `canceled`.

**A client disconnect does not cancel an A2A task.** This is deliberately the opposite of `/invocations`' behaviour: `tasks/resubscribe` exists precisely so a dropped stream can be reattached, and cancelling on disconnect would mean a resubscribing client could only ever find cancelled tasks. Only `tasks/cancel` produces `canceled`.

**Tasks live in memory by default and are lost when the container stops.** `AgentCoreServerBuilder::task_store` takes any `TaskStore` implementation — back it with a database for deployments whose clients rely on `tasks/get`/`tasks/resubscribe` across container lifetimes.

The agent card is derived from the configured agent (name, description, one matching skill), reports *this crate's* version — a library cannot read its host binary's — and **omits `url`** unless `AGENTCORE_RUNTIME_URL` or `AgentCoreServerBuilder::agent_card_url` supplies one, since `0.0.0.0` is a bind address rather than somewhere a client can connect. `AgentCoreServerBuilder::agent_card` replaces the derived card wholesale.

```bash
cargo run -p paigasus-helikon-runtime-agentcore --example a2a_server --features a2a
```

## AG-UI-protocol mode (feature `ag-ui`, default on; `AgentCoreServer::serve_agui`)

AgentCore's **AG-UI runtime type**: AG-UI's event vocabulary over SSE and WebSocket.

| | |
| --- | --- |
| Bind address | `0.0.0.0:8080` — the same port as the HTTP protocol, per AWS's contract |
| Endpoints | `POST /invocations` (SSE), `GET /ws`, `GET /ping` |
| Request body | AG-UI's `RunAgentInput`. Unknown fields (`tools`, `context`, `state`, `forwardedProps`) are accepted and ignored |
| Response | AG-UI events: `RUN_STARTED`, `TEXT_MESSAGE_*`, `THINKING_TEXT_MESSAGE_*`, `TOOL_CALL_*`, `STEP_*`, `RUN_FINISHED`/`RUN_ERROR` |

AG-UI and the HTTP protocol are alternative `serverProtocol` settings for one container and share both port and path, so a deployment runs one or the other — and `agui_router()`/`router()` must never both be merged into one app.

`/invocations` here is **SSE only**: it does not honour `Accept: application/json`, because the AG-UI contract defines no buffered form.

**AG-UI mode is stateless per request and cannot use a persistent session backend in v0.** AG-UI clients resend the whole conversation each request while the runner seeds the model with `history ++ input.messages`, so pairing a persisted session with a full client history would double-count every prior turn. Each request therefore gets a fresh, unshared session, and `messages` is treated as the entire conversation. The session header is still validated and used only as a fallback source for `threadId`.

**Concurrent agents map imperfectly.** AG-UI's text and tool-call events assume one active span at a time, so an agent interleaving two tool calls has its spans serialized onto the wire rather than genuinely nested.

```bash
cargo run -p paigasus-helikon-runtime-agentcore --example agui_server --features ag-ui
```

## WebSocket frame quotas

Both WebSocket endpoints pace and split outbound frames to stay inside AgentCore's documented limits (64 KB per frame, 250 frames/second), budgeting against the conservative reading of each. Splitting keeps a frame a valid protocol event wherever it can — a long text delta becomes several smaller deltas — and falls back to `helikon.chunk` envelopes only for events whose payload cannot be split into several valid events. A client that never sends oversize payloads never sees an envelope; one that might must reassemble `helikon.chunk` frames in `seq` order until `final` is `true`.

## Feature flags

| Feature | Default | Enables |
| --- | --- | --- |
| `mcp` | ✅ | MCP-protocol mode (`serve_mcp`, port 8000) |
| `a2a` | ✅ | A2A-protocol mode (`serve_a2a`, port 9000) + `TaskStore` |
| `ag-ui` | ✅ | AG-UI-protocol mode (`serve_agui`, port 8080) |
| `ws` | ✅ | The optional `GET /ws` endpoint on the HTTP protocol |
| `example-anthropic` | | Pulls the Anthropic provider in for `examples/agent_http.rs` only |

A deployment serving one protocol pays for all four by default. Opt out to compile the rest away:

```bash
cargo add paigasus-helikon-runtime-agentcore --no-default-features --features a2a
```

## MCP-protocol mode (feature `mcp`, default on; `AgentCoreServer::serve_mcp`)

AgentCore also supports an **MCP runtime type**: instead of the HTTP-protocol contract above, the container serves the configured agent as a single MCP tool over rmcp's streamable-HTTP transport.

| | |
| --- | --- |
| Bind address | `0.0.0.0:8000` (fixed, distinct from HTTP mode's 8080) |
| Endpoint | `POST /mcp` — streamable-HTTP, **stateless mode** |
| Session header | `Mcp-Session-Id`, platform-injected and never initialized by this server — stateless mode is required so an unrecognized, platform-generated id is accepted rather than rejected |
| Bonus | A trivial `GET /ping` also answers on port 8000 (not part of MCP itself; cheap insurance for probes that expect *something* there) |

`AgentCoreServer::mcp_router()`/`serve_mcp()` configure rmcp with `with_legacy_session_mode(false)` and `disable_allowed_hosts()` — the latter because rmcp's DNS-rebinding guard defaults to accepting only a loopback `Host` header, and real AgentCore traffic arrives from inside the platform's microVM with an arbitrary one; AgentCore's microVM boundary is the actual network perimeter here, not this in-process check.

(rmcp 3 renamed `with_stateful_mode` to `with_legacy_session_mode`. Per SEP-2567 sessions are removed from protocol version `2026-07-28`, so a client negotiating that version is served statelessly regardless of the flag; it still governs the legacy `< 2026-07-28` path, which is where the platform-injected session id would otherwise be rejected.)

```bash
cargo run -p paigasus-helikon-runtime-agentcore --example mcp_server --features mcp
```

## Docker image

`docker/Dockerfile` is a multi-stage build: a `rust:1.94-alpine` (musl) builder stage produces a statically linked `aarch64-unknown-linux-musl` binary, stripped and shipped alone in a `FROM scratch` final image (no libc, no shell, nothing else). Build context must be the workspace root (it's a Cargo workspace).

The builder stage's `RUN` uses a BuildKit cache mount, so BuildKit is required — the default since Docker 23; set `DOCKER_BUILDKIT=1` if it's been disabled in your environment, otherwise the build fails with `dockerfile parse error … Unknown flag: mount`.

```bash
# Minimal-overhead image (no model provider, no TLS stack):
docker build --platform linux/arm64 \
  -f crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile \
  --build-arg EXAMPLE=echo_http \
  -t helikon-agentcore-echo .

# Model-backed image (the size/cold-start acceptance-criteria image):
docker build --platform linux/arm64 \
  -f crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile \
  --build-arg EXAMPLE=agent_http --build-arg FEATURES=example-anthropic \
  -t helikon-agentcore-agent .
```

`linux/arm64` is hardcoded because AgentCore's own runtime targets are arm64 microVMs — AgentCore images are **linux/arm64 mandatory**, private-ECR, ≤ 2 GB.

Measured on Docker Desktop 29.5.3, native arm64 macOS (`scripts/agentcore-image-check.sh`; see the [runbook](https://github.com/SMK1085/paigasus-helikon/blob/main/docs/runbooks/agentcore-image-check.md) for the full procedure):

| Metric | Value | Gate |
| --- | ---: | --- |
| `helikon-agentcore-echo` image size (AC gate) | 1.31 MB | < 30 MB |
| `helikon-agentcore-agent` image size (AC gate) | 3.27 MB | < 30 MB |
| echo `exec`→`/ping`-200 (AC gate) | 11 ms | < 50 ms |
| agent `exec`→`/ping`-200 (AC gate) | 9 ms | < 50 ms |

Both gates passed with wide margin. Neither number includes AWS's own microVM provisioning latency (documented by AWS as roughly 2–5 seconds) — that happens entirely platform-side, before AgentCore ever execs the container's entrypoint, and is outside this crate's control.

## Deploying with AWS CDK

AWS publishes a stable L2 construct library for AgentCore Runtime: `aws-cdk-lib/aws-bedrockagentcore` (`Runtime`, `AgentRuntimeArtifact`, `ProtocolType`). Push the image built above to a private ECR repository, then:

```typescript
import * as ecr from 'aws-cdk-lib/aws-ecr';
import * as agentcore from 'aws-cdk-lib/aws-bedrockagentcore';

const repository = ecr.Repository.fromRepositoryName(this, 'AgentRepo', 'my-agent-repo');
const artifact = agentcore.AgentRuntimeArtifact.fromEcrRepository(repository, 'v1.0.0');

// HTTP-protocol mode (`AgentCoreServer::serve`, port 8080) — the default;
// omit `protocolConfiguration` entirely for this mode.
const runtime = new agentcore.Runtime(this, 'MyAgentRuntime', {
  runtimeName: 'myAgent',
  agentRuntimeArtifact: artifact,
});

runtime.addEndpoint('production', {
  version: '1',
  description: 'Stable production endpoint — pinned to v1',
});

// A non-default protocol instead sets `protocolConfiguration` on the `Runtime`
// props above — everything else (artifact, endpoint) is unchanged:
//   protocolConfiguration: agentcore.ProtocolType.MCP,    // serve_mcp,  port 8000
//   protocolConfiguration: agentcore.ProtocolType.A2A,    // serve_a2a,  port 9000
//   protocolConfiguration: agentcore.ProtocolType.AG_UI,  // serve_agui, port 8080
```

## Abrupt termination and session persistence

AgentCore gives no documented `SIGTERM` contract — termination (idle timeout, max lifetime, or scale-down) can be abrupt. **In HTTP-protocol mode**, durable conversation state belongs in the configured `Session` backend (e.g. `paigasus-helikon-sessions-sqlite`/`-postgres`/`-redis` via a custom `SessionProvider`), never in container memory — the default `InMemorySessionProvider` loses everything on termination, same as any in-process cache. **In A2A-protocol mode**, the same applies twice over: conversation state belongs in the `Session` backend, and A2A *tasks* belong in a durable `TaskStore` (`AgentCoreServerBuilder::task_store`) if clients rely on `tasks/get`/`tasks/resubscribe` surviving a restart — the default `InMemoryTaskStore` loses every task with the microVM. **In AG-UI-protocol mode**, persistence is unavailable in v0: every request gets a fresh, unshared session (see that section above). **In MCP-protocol mode**, this guidance likewise does not apply: `AgentCoreServer::mcp_router`/`serve_mcp` always give the wrapped `McpAgentServer` a fresh, unshared in-memory session per call (mirroring `paigasus-helikon-mcp`'s own per-call-context design) and do not consult this server's configured session/context providers at all — MCP mode cannot use a persistent session backend in v0.

## Session keys carry no principal in this runtime

This crate reuses `paigasus-helikon-runtime-axum`'s `SessionProvider` trait, whose
`session` method now takes a `SessionKey<'_>` (principal + caller-supplied id)
instead of a bare `Option<&str>` — see that crate's README for the full
migration story. In *this* runtime the key's `principal` is always `None`:
AgentCore exposes no `AuthLayer` seam, and each session already runs in its own
microVM instance, so the validated session id is the whole identity here. A
custom `SessionProvider` supplied via `AgentCoreServerBuilder::session_provider`
must not expect principal-based separation — including via
`SessionKey::storage_key`, which reduces to a stable per-id key when the
principal is absent. That is the intended behaviour, not an oversight; see the
crate docs (§ "Session keys carry no principal in this runtime") for the full
rationale.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-runtime-agentcore)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [Runtimes](https://smk1085.github.io/paigasus-helikon/concepts/runtimes.html)
- [Image size/cold-start runbook](https://github.com/SMK1085/paigasus-helikon/blob/main/docs/runbooks/agentcore-image-check.md)
- [AWS Bedrock AgentCore documentation](https://docs.aws.amazon.com/bedrock-agentcore/)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.

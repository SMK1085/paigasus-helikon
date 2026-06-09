# SMA-327 — `paigasus-helikon-mcp`: rmcp client + server wrapper (design)

First-class MCP support in both directions: connect to external MCP servers as
tool sources, and expose any `Agent<Ctx>` as an MCP server. Thin wrapper around
`rmcp` per the ADR *MCP is the canonical tool ABI, via rmcp*.

## Decisions (vs. the original ticket text)

| Topic | Ticket said | Decided | Why |
|---|---|---|---|
| rmcp version | `0.16+`, 2025-06-18 spec | **`rmcp = "1.7"`** (`^1.7` in `[workspace.dependencies]`) | 0.16 predates rmcp 1.0 (2026-03); eight 1.x releases since. Building new code on a year-old API buys an immediate migration ticket. An `=1.7.x` pin was considered and rejected: exact pins in a *published* library force resolution conflicts on downstreams using any other rmcp 1.x and block patch fixes. `Cargo.lock` pins our builds; semver guards the 1.x line; CI catches churn. |
| SSE client transport | builder for `sse` | **Dropped** | rmcp removed SSE transports in 0.11.0 (PR #562); streamable HTTP is the only HTTP transport in 1.x. SSE was deprecated by the 2025-06-18 spec revision itself. |
| MSRV | — (workspace 1.75) | **Per-crate `rust-version = "1.85"` on `paigasus-helikon-mcp` only**; workspace stays 1.75 (revised from an earlier workspace-wide bump after design review) | rmcp 1.x is edition 2024 (requires ≥ 1.85), but raising the floor for every crate because of one opt-in feature-gated crate punishes downstreams that never enable it. Core/providers/facade-default keep the 1.75 guarantee; only the mcp crate (and the facade *with* `mcp` enabled) require 1.85. CI details below. |
| Builder integration | sketch showed `.mcp_servers([...])` on `LlmAgent` builder | **Explicit `handle.tools::<Ctx>().await?` passed to the existing `.tools(...)`** | Core cannot depend on the mcp crate, and `.build()` is sync while discovery is async. Zero core changes keeps this ticket self-contained; builder sugar (a `ToolSource` trait in core) is a possible follow-up. |
| `lazy` semantics | "defers schema fetch until a tool is invoked" | **Search meta-tool pattern** (see below) | MCP's `tools/list` returns names *and* schemas in one call — there is no separate schema fetch to defer. The 6,000-tool problem is model-context economy, not wire traffic. |

## Architecture

Thin stateless wrapper. No reconnect logic, no connection actor: rmcp's
`RunningService` already owns the connection task; a dropped connection
surfaces as a `ToolError` and the caller reconnects. The crate depends on no
Helikon crate other than `paigasus-helikon-core` (no runtime-tokio coupling);
third-party deps are `rmcp`, `axum` (HTTP binding), and the usual support
crates (see Dependencies).

```text
McpServerHandle (Clone)
 └─ Arc<Inner>
     ├─ RunningService<RoleClient, ()>     // rmcp connection task
     ├─ cached Vec<rmcp::model::Tool>      // from list_all_tools() at connect
     └─ config (lazy, tool_prefix, …)

McpTool<Ctx>   ─holds─> McpServerHandle    // connection lives while any tool lives
search_tools   ─holds─> McpServerHandle    // lazy-mode meta tool
McpAgentServer<Ctx> ──> Arc<dyn Agent<Ctx>> + ctx factory + RunConfig
```

Module layout: `lib.rs` (crate docs, re-exports), `error.rs`,
`client/handle.rs`, `client/tool.rs`, `client/search.rs`, `server.rs`.

## Client side: `McpServerHandle`

```rust
let fs = McpServerHandle::stdio(Command::new("npx"), |cmd| {
        cmd.args(["-y", "@modelcontextprotocol/server-filesystem", "/data"]);
    })
    .lazy(true)              // default false
    .tool_prefix("fs_")      // optional; avoids cross-server name collisions
    .connect().await?;       // serve + initialize + list_all_tools

let http = McpServerHandle::streamable_http("https://api.example.com/mcp")
    .auth_header(token)      // optional knobs forwarded to rmcp's transport config
    .connect().await?;

// explicit-lifecycle escape hatch: bring a fully configured transport
let cp = McpServerHandle::child_process(transport /* TokioChildProcess */)
    .connect().await?;

let agent = LlmAgent::builder()
    .name("research")
    .model(model)
    .tools(fs.tools::<MyCtx>().await?)
    .build()?;
```

- `stdio` and `child_process` both ride rmcp's `TokioChildProcess` (one
  transport in 1.x); `child_process` accepts a pre-built transport for full
  lifecycle control (`TokioChildProcess::builder()`), satisfying the ticket's
  explicit-lifecycle distinction.
- Connect uses the unit client handler: `().serve(transport)`. Tool cache is
  fetched once at connect via `list_all_tools()` (auto-paginating).
- `close(self).await` cancels the connection explicitly; dropping the last
  clone tears it down (child processes are killed via process-wrap).
- Errors before/at connect surface as `McpError`.

### `McpTool<Ctx>` (implements core `Tool<Ctx>`)

- `name()` — prefixed wire name. `description()` — server-provided or `""`.
- `schema()` / `output_schema()` — owned `serde_json::Value`s cloned from
  `input_schema` / `output_schema` at construction (satisfies the `&Value`
  return type).
- `effect()` — `annotations.read_only_hint == Some(true)` → `ToolEffect::ReadOnly`,
  else `ToolEffect::SideEffect` (MCP's `destructive_hint` defaults to true, so
  side-effect is the safe default). `ToolEffect::Write` is **never** produced:
  server-declared hints are untrusted metadata and must not unlock
  `AcceptEdits` auto-approval. Documented on `McpTool` so the
  `Plan`/`AcceptEdits` interaction is no surprise.
- `invoke()` — args must be a JSON object or null (`ToolError::InvalidArgs`
  otherwise); strips the prefix; `peer.call_tool(CallToolRequestParams::new(name)
  .with_arguments(obj))`. Result mapping:
  - `is_error == Some(true)` → `ToolError::Other` carrying the text content;
  - else `ToolOutput.content` = `structured_content` if present, else a single
    text content as `Value::String`, else the content vec serialized as a JSON
    array.
- `Ctx` is a phantom — MCP tools never read user context, so `tools::<Ctx>()`
  adapts to any agent's context type.

### Lazy mode (`.lazy(true)`)

`tools::<Ctx>()` returns the same `McpTool`s but advertising placeholder schema
`{"type":"object","additionalProperties":true}`, plus one `search_tools`
meta-tool (also prefixed; `ReadOnly`):

- input: `{ "query": string }`;
- behavior: substring/keyword match over cached tool names + descriptions;
- output: matching tools' real names, descriptions, and full input schemas as
  JSON.

The model searches, reads the schema from the tool output, then calls the real
tool. Schemas were already delivered by `tools/list`, so lazy mode costs no
extra wire calls — it is purely model-context economy (the OpenAI/Claude-Code
tool-search pattern).

## Server side: `McpAgentServer<Ctx>`

```rust
let server = McpAgentServer::new(agent)        // agent: impl Agent<Ctx> + 'static
    .name("paigasus-triage")
    .version("1.0.0")
    .instructions("…")                          // optional MCP instructions
    .with_ctx(|| AppCtx::connect())             // Fn() -> Ctx + Send + Sync, per request
    .with_run_config(RunConfig::new());         // optional (timeout, …)

// when Ctx: Default
let server = McpAgentServer::with_default_ctx(agent);

server.serve_stdio().await?;                          // blocks until disconnect
server.serve_streamable_http("0.0.0.0:8000").await?;  // axum bind, blocks
let svc = server.streamable_http_service();           // tower-service escape hatch
```

Implements rmcp's `ServerHandler` manually (no `#[tool]` macros — the tool list
is derived from the wrapped agent):

- `get_info` — tools capability, `Implementation::new(name, version)`, optional
  instructions.
- `list_tools` — exactly one tool: name = agent name sanitized to
  `[a-zA-Z0-9_-]`, description = `agent.description()`, input schema
  `{"type":"object","properties":{"input":{"type":"string"}},"required":["input"]}`.
  No output schema (agent output is free text).
- `call_tool` — parse `input`; build
  `RunContext::new(Arc::new(factory()), Arc::new(MemorySession::new()),
  HookRegistry::new(), TracerHandle::builder().build(), cancel_token)`;
  drive `agent.run(ctx, input)` through core's
  `RunResultStreaming::new(stream).collect()` (no runtime-tokio dependency).
  Final text → `CallToolResult::success([Content::text(…)])`; a failed run →
  `CallToolResult::error(…)` (tool-level error the calling model can react to),
  not a protocol error.
- **Execution control** (`collect()` does not enforce `RunConfig::timeout` —
  timeout is runner-scoped per SMA-321, and only `TokioRunner` honors it):
  - the `collect()` is wrapped in `tokio::time::timeout(run_config.timeout, …)`
    when a timeout is configured; expiry cancels the token and returns
    `CallToolResult::error("run timed out")`;
  - `cancel_token` is a child of rmcp's per-request cancellation
    (`RequestContext<RoleServer>.ct`), so a client disconnect or MCP
    `notifications/cancelled` aborts the agent run instead of leaving it
    executing unbounded — without this, an externally exposed server is a
    resource/DoS hazard.
- HTTP serving: rmcp's `StreamableHttpService` (tower) bound via axum 0.8 with
  `LocalSessionManager` (stateful sessions; one handler clone per session).
  Users with their own router mount `streamable_http_service()` instead.

## Errors

One `McpError` enum (`thiserror`, workspace convention): `Connect`, `Spawn`
(child-process io), `Service(rmcp::ServiceError)`, `Bind`, `Serve`, and
`Other(#[from] anyhow::Error)`. Client-side tool failures surface as core
`ToolError`, never `McpError` — agents only ever see the `Tool` trait.

## Dependencies

```toml
paigasus-helikon-core = { workspace = true }
rmcp = { workspace = true }   # workspace pin "1.7", default-features = false, features:
                              # client, server, transport-io, transport-child-process,
                              # transport-streamable-http-client-reqwest, reqwest (rustls),
                              # transport-streamable-http-server
async-trait, serde_json, thiserror, anyhow, tokio, futures, tokio-util, axum
```

### Supply-chain impact

This crate adds a material **production** dependency tree (rmcp + axum +
reqwest/rustls + hyper/h2) to the workspace graph that the **required**
`deny`/`audit` gates scan (`cargo deny --all-features check`). Known landmine:
the rustls **crypto backend** — `aws-lc-rs` (its `aws-lc-sys` carries an
`OpenSSL` license term) and older `ring` (`ISC AND MIT AND OpenSSL`) are **not**
covered by the current `deny.toml` allowlist (`Apache-2.0`, `MIT`, `BSD-2/3`,
`ISC`, `MPL-2.0`, `Unicode-*`, `Zlib`). Plan obligations:

1. Select the rustls crypto backend **explicitly** via reqwest/rmcp features so
   the graph is deterministic (don't inherit the default silently).
2. Run `cargo deny check` and `cargo audit` locally with the mcp crate in the
   graph **early in implementation**, and resolve what surfaces — expected: an
   allowlist/clarification entry for the chosen backend's license in
   `deny.toml` (with a comment, matching the existing `Unicode-3.0`/`Zlib`
   precedent), or a backend choice whose license is already allowed.
3. The advisory surface grows permanently (published crate). The daily
   `scheduled-audit` job will now watch rmcp/hyper/h2/rustls advisories — this
   is accepted, not accidental.

## Testing

All in-process over `tokio::io::duplex` except the npx acceptance test:

1. `tests/client_tools.rs` — fixture rmcp `ServerHandler`: schema/description
   fidelity, prefixing, effect mapping, invoke round-trip, `is_error` →
   `ToolError`, non-object args rejected.
2. `tests/lazy.rs` — placeholder schemas, `search_tools` returns real schemas,
   prefix interplay.
3. `tests/agent_server.rs` — `McpAgentServer` over duplex driven by a raw rmcp
   client: list/call, ctx factory invoked per request, run failure → `is_error`;
   execution control: a configured timeout expires → `is_error` ("run timed
   out") and the run's cancel token fires; request cancellation propagates into
   the running agent.
4. `tests/roundtrip.rs` — **AC2 in-process**: `LlmAgent` (scripted fake model)
   served via `McpAgentServer`, consumed through `McpServerHandle` — both
   halves exercising each other ("second Paigasus instance").
5. `tests/acceptance_filesystem.rs` — **AC1**, `#[ignore]`: connects to
   `@modelcontextprotocol/server-filesystem` via npx, asserts tools load with
   schemas intact. Run locally before the PR.

## Workspace / CI / release fallout (single PR)

- `[workspace.dependencies]`: `rmcp = "0.16"` → `"1.7"`; the
  `paigasus-helikon-mcp` internal pin gains `version = "0.1.0"`.
- MSRV (scoped to the mcp crate, not workspace-wide):
  - `crates/paigasus-helikon-mcp/Cargo.toml` replaces
    `rust-version.workspace = true` with a literal `rust-version = "1.85"`;
    `[workspace.package] rust-version` **stays `1.75`** (second documented
    inheritance exception after the CLI's lints override — CLAUDE.md updated
    to record both the exception and the rationale).
  - `ci.yml` 1.75 matrix legs keep running but change args: cargo has no
    "all-features-except", so the 1.75 leg becomes
    `--workspace --exclude paigasus-helikon-mcp` with the facade's features
    **enumerated minus `mcp`** (e.g.
    `--features paigasus-helikon/openai,paigasus-helikon/anthropic,…`) instead
    of `--all-features`. The stable legs keep `--all-features` and cover the
    mcp crate at its real MSRV.
  - `msrv.yml` (verifies core at 1.75) is unaffected — core doesn't touch rmcp.
  - **Plan-time verification gate**: confirm with CI's *exact* command that
    cargo 1.75 tolerates an edition-2024 rmcp sitting in `Cargo.lock` while
    excluded from the build (the SMA-404 lesson: reproduce CI's command before
    claiming a 1.75 break or non-break). If resolution itself fails at 1.75,
    fall back to the workspace-wide 1.85 bump and say so in the PR.
- Release: 4-step ascend for `paigasus-helikon-mcp` (0.0.0 → 0.1.0, drop
  `publish = false`, drop the `release-plz.toml` block, `chore(release)`
  commit) **plus a facade patch bump** (version + workspace self-pin +
  CHANGELOG) — the same-PR manual bump defeats `dependencies_update`, and
  without the facade bump the published facade keeps requesting
  `paigasus-helikon-mcp = ^0.0.0` (the SMA-346 trap). Core is untouched — no
  core bump.
- Branch: `feature/sma-327-paigasus-helikon-mcp-rmcp-client-server-wrapper`.
  PR title: `feat(mcp): SMA-327 add rmcp client + server wrapper`.

## Acceptance criteria mapping

- *Filesystem server loads tools* → test 5 (`#[ignore]`, run locally) backed by
  test 1 (in-process equivalent, runs in CI).
- *Agent exposed via `McpAgentServer` callable from another MCP-aware client* →
  test 4 (in-process round-trip through our own client wrapper).

## Out of scope (follow-ups)

- `ToolSource` trait in core + `.mcp_servers(...)` builder sugar — restores
  the planned ergonomic; **needs a Linear ticket** (requires a same-PR core
  bump + facade bump when implemented).
- Reconcile the Notion "MCP Integration" page with this spec's deviation
  table (explicit `.tools(...)`, streamable-HTTP-only, corrected
  lazy/`search_tools` description) — the page still shows the superseded
  design.
- Reconnect/backoff, `tools/list_changed` subscription, health checks.
- SSE transport (only if a concrete SSE-only server shows up).
- MCP resources/prompts (tools only for now); sampling/elicitation handlers.
- Multi-tool agent serving (expose handoffs/sub-agents as separate MCP tools).

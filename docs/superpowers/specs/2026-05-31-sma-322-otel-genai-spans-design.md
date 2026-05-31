# SMA-322 — OpenTelemetry spans with GenAI semantic conventions

**Status:** Design (approved)
**Issue:** [SMA-322](https://linear.app/smaschek/issue/SMA-322)
**Branch:** `feature/sma-322-opentelemetry-spans-with-genai-semantic-conventions`
**Date:** 2026-05-31
**Milestone:** MVP · **Labels:** `area:observability`, `stage:1`
**References:** Notion *Observability & Evaluation* (`355830e8fbaa81869381f202ca03fee7`); ADR *OpenTelemetry-native observability with GenAI semantic conventions*.

## 1. Summary

Add built-in OpenTelemetry instrumentation to the agent loop following the **current**
GenAI semantic conventions, so Langfuse / Datadog / Jaeger / Tempo / Honeycomb
auto-recognize the telemetry against the *standard* OTLP exporter — no custom exporter
code in the SDK.

The library emits **plain `tracing` spans** (zero-cost when no subscriber is
installed — the tokio/hyper convention). The OTel → OTLP → Langfuse export half lives
**entirely in a runnable example** that uses dev-dependencies. Net new *production*
dependency surface is **zero**: `tracing = "0.1"` is already a dependency of `core`,
`providers-openai`, and `providers-anthropic`.

This honors the ticket's "without any custom exporter" goal precisely: because the
spans carry standard GenAI-semconv span names and `gen_ai.*` attribute keys, an
off-the-shelf `opentelemetry-otlp` exporter pointed at any backend ingests them and
the backend classifies them as GenAI generation/tool/agent spans. The SDK's
deliverable is the *instrumentation*; the *wiring* is demonstrated, not shipped as
library code.

### 1.1 Reconciliation: this supersedes the ticket's stale attribute names

The Linear ticket and the Notion page were written against an **earlier GenAI semconv
draft** and name `gen_ai.system`, `agent.name`, `tool.name`, and literal span names
`agent.run` / `agent.turn` / `gen_ai.chat` / `tool.execute`. Verified against the live
semconv (2026), the current names differ (see §5). This design adopts the **current**
semconv and supersedes the stale names — the same stale-docs reconciliation done for
SMA-321/ADR-13. Rationale: the headline AC is *backends recognize the telemetry out of
the box*, which only holds with current names; and because span names + attribute keys
become part of users' saved queries/dashboards, fixing them post-ship is a breaking
change, so greenfield-now is far cheaper. The Linear ticket and the Notion ADR/page are
reconciled in step with this design.

## 2. Scope

### In scope (the four live execution paths)

```
invoke_agent {agent}     (INTERNAL)   run span — gen_ai.operation.name = invoke_agent
└─ agent.turn            (INTERNAL)   custom span — no semconv equivalent
   ├─ chat {model}       (CLIENT)     model call — gen_ai.operation.name = chat
   └─ execute_tool {tool}(INTERNAL)   one per concurrent call — operation.name = execute_tool
```

* `tracing` spans for the agent run, each turn, each model call, and each tool call.
* GenAI-semconv attributes: `gen_ai.operation.name`, `gen_ai.provider.name`,
  `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`,
  `gen_ai.agent.name`, `gen_ai.tool.name`.
* Langfuse-compatible trace-level attributes: `langfuse.session.id`,
  `langfuse.user.id`, `langfuse.trace.tags`, carried on `TracerHandle`.
* A runnable example wiring the Langfuse OTLP exporter (incl. the tags-array span
  processor, §7).

### Out of scope (deferred to their own tickets)

* **An agent-handoff span.** The handoff path returns `AgentError::NotImplemented`
  (`loop_state.rs:499`, `agent.rs:867`). Instrumenting it now would wrap a span around
  a path that only ever errors. Documented as a named seam; wired by the handoff
  ticket.
* **Guardrail / approval spans.** Guardrails are "stored but not driven"
  (`agent.rs:248-251`); approval returns `NotImplemented` (`loop_state.rs:501`).
* **A library-side exporter/init helper or a new `-telemetry` crate.** The example *is*
  the wiring (Decision 1).
* **True OTel Baggage cross-service propagation.** Handle-stamping is used for the MVP
  (Decision 3); cross-process Baggage is a named follow-up.

## 3. Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | **Exporter wiring lives in an example app only** (dev-deps), not a library helper or new crate. | No new *production* deps. **Caveat:** the OTLP dev-deps DO enter the `cargo deny` / `cargo audit` graph — both scan dev-deps and `deny.toml` has no dev-exclusion. §8.1 adds a gate to run them and resolve license findings (notably the reqwest→rustls→`ring` TLS chain) before merge. SBOM uses `cargo-cyclonedx --all-features` on the facade (published graph), so it is expected unaffected — confirmed in §8.1. |
| 2 | **Instrument only the four live paths**; leave handoff/approval/guardrail as documented seams. | Everything emitted is end-to-end verifiable now; nothing instruments code that returns `NotImplemented`. |
| 3 | **Trace-level metadata is handle-stamped, pure-`tracing`.** `TracerHandle` carries `session_id` / `user_id` / `tags`; the run span stamps the `langfuse.*` attributes and `agent.turn` re-stamps them. | Langfuse propagates trace-level attributes from the root to child observations, so stamping the run span (+ turn) is sufficient in-process — no OTel Baggage plumbing in `core`. The attributes are stamped on the run + turn spans, **not** literally every span; Langfuse handles child propagation. Cross-service Baggage is a follow-up. |
| 4 | **`gen_ai.provider.name` / `gen_ai.request.model` via two `Model` trait getters** (`provider()` / `model()`) with default impls. | `core` cannot otherwise learn these — the model id is configured inside each provider and never crosses into `ModelRequest`. Default impls keep external `Model` implementors compiling; `ModelCapabilities` is `Copy`/flags-only and a poor carrier for a runtime `String`. |
| 5 | **`langfuse.trace.tags`: `core` records a JSON-array string field; the example's span processor rewrites it to a native `string[]` before export.** | Langfuse requires tags as a native `string[]` (a JSON-string silently fails to register). Doing the array conversion in the example keeps `core` `tracing`-only (no `tracing-opentelemetry` dep in `core`) while making tags actually work. `langfuse.session.id` / `langfuse.user.id` are scalar strings — recorded directly. |
| 6 | **Adopt the current GenAI semconv names**, superseding the ticket's stale draft names (§1.1, §5). | The AC requires backends to recognize the telemetry; only current names achieve that. Greenfield, so no breakage. |

## 4. Architecture & seams

All three seams already exist in `core`:

* **`TracerHandle`** — `core/src/context.rs:253`, today a placeholder unit struct
  annotated *"gains real fields with the observability ticket."* This is that ticket.
  Already threaded into `RunContext` (`context.rs:53`) and projected into
  `ToolContext` (`context.rs:143-149`).
* **`LlmAgent::run()`** — `core/src/agent.rs:584`, the `async_stream` that already
  yields `RunStarted → TurnStarted → …deltas… → RunCompleted { usage }`. Spans open
  and close here.
* **`run_tools_concurrent`** — `core/src/agent.rs:488`, where per-call tool spans wrap
  each `tool.invoke`.

### 4.1 Span-guard mechanics

An `Entered` guard from `Span::enter()` is `!Send` and must **never** be held across
an `.await` or `yield` — both pervasive in the `async_stream`. The design therefore
**never enters spans**:

* Hold `tracing::Span` **handles** (which are `Send + Sync`) in loop variables across
  turns; close a span by **dropping its handle** (drop → OTel span end time).
* Build the hierarchy with **explicit parents**:
  `info_span!(parent: &turn_span, …)`. Explicit parenting does *not* require the
  parent to be entered.
* Wrap discrete async futures with `.instrument(span)` where it composes cleanly —
  specifically each per-tool `tool.invoke` future inside `run_tools_concurrent`
  (`Instrument` is `Send`-safe and enters/exits around each poll).
* Deferred attributes (token counts, status) are declared `tracing::field::Empty` at
  creation and filled with `span.record("…", value)` before the handle is dropped.
* **Dotted field names are first-class in `tracing`**: the macros treat
  `gen_ai.request.model = %m` as a single field named `"gen_ai.request.model"`, and
  `tracing-opentelemetry` maps that name verbatim to the OTel attribute key.

### 4.2 Dynamic span names via `otel.name`

Semconv span names are **dynamic** (`chat {model}`, `execute_tool {tool}`,
`invoke_agent {agent}`), but a `tracing` span's macro name must be a `'static str`.
Resolution: give each span a stable static `tracing` name and set the **exported** OTel
name via the `otel.name` special field that `tracing-opentelemetry` honors — declared
`field::Empty` at creation and recorded once the dynamic part is known
(`span.record("otel.name", format!("chat {model}"))`). Likewise `otel.kind` and
`otel.status_code` / `otel.status_message` are `tracing-opentelemetry` special fields.

## 5. Span tree & attribute reference

Empty/`"unknown"` attribute values are elided. `otel.name` carries the dynamic semconv
span name (§4.2); the `tracing` macro name is the stable token in parentheses.

### Run span — `tracing` name `agent.run`, `otel.name = "invoke_agent {agent}"`, `otel.kind = internal`
| Attribute | Value / source | When |
|-----------|----------------|------|
| `gen_ai.operation.name` | `"invoke_agent"` (Required) | open |
| `gen_ai.agent.name` | active agent name | open |
| `langfuse.session.id` | `TracerHandle::session_id()` | open |
| `langfuse.user.id` | `TracerHandle::user_id()` | open |
| `langfuse.trace.tags` | `TracerHandle::tags()`, JSON-array string → native array at export (§7) | open |
| `gen_ai.usage.input_tokens` / `output_tokens` | aggregate from `RunCompleted { usage }` | close |
| `otel.status_code` (+ `otel.status_message`) | `ERROR` on `RunFailed` | close |

### Turn span — `tracing` name `agent.turn`, `otel.kind = internal` (custom; no semconv equivalent)
| Attribute | Value / source | When |
|-----------|----------------|------|
| turn index (span field) | loop turn counter | open |
| `langfuse.session.id` / `user.id` / `trace.tags` | re-stamped from `TracerHandle` (Langfuse child propagation) | open |

No `gen_ai.operation.name` — `"turn"` is not a defined operation value.

### Model-call span — `tracing` name `gen_ai.chat`, `otel.name = "chat {model}"`, `otel.kind = client` (one per `NextAction::CallModel`)
| Attribute | Value / source | When |
|-----------|----------------|------|
| `gen_ai.operation.name` | `"chat"` (Required) | open |
| `gen_ai.provider.name` | `Model::provider()` (e.g. `openai`, `anthropic`) | open |
| `gen_ai.request.model` | `Model::model()` | open |
| `gen_ai.usage.input_tokens` / `output_tokens` | the loop's existing `latest_usage` for this invocation | close |
| `otel.status_code` | `ERROR` on model error | close |

> Span name is `chat {model}`. A future refinement may emit `text_completion` /
> Responses-flavored operations when a provider reports it via
> `capabilities().server_managed_state`; out of scope here.

### Tool span — `tracing` name `tool.execute`, `otel.name = "execute_tool {tool}"`, `otel.kind = internal` (one per call, parented to the turn)
| Attribute | Value / source | When |
|-----------|----------------|------|
| `gen_ai.operation.name` | `"execute_tool"` (Required) | open |
| `gen_ai.tool.name` | `ToolCallRequest::name` | open |
| `otel.status_code` (+ `otel.status_message`) | `ERROR` when the outcome is `Err` | close |

**Token-count acceptance criterion** is satisfied directly: the model-call span records
the per-invocation `latest_usage` already tracked at `agent.rs:638-685`; the run span
records the aggregate from `RunCompleted { usage }`. No new accounting is introduced.

## 6. Public API changes

### 6.1 `TracerHandle` (`core/src/context.rs`) — additive, non-breaking

```rust
/// Carrier for per-run trace-level attributes stamped onto the run/turn spans.
#[derive(Debug, Clone, Default)]
pub struct TracerHandle {
    session_id: Option<String>,
    user_id: Option<String>,
    tags: Vec<String>,
}

impl TracerHandle {
    pub fn builder() -> TracerHandleBuilder { /* … */ }
    pub fn session_id(&self) -> Option<&str> { /* … */ }
    pub fn user_id(&self) -> Option<&str> { /* … */ }
    pub fn tags(&self) -> &[String] { /* … */ }
}

/// Consuming builder: `with_session_id` / `with_user_id` / `with_tag` / `build`.
#[derive(Debug, Default)]
pub struct TracerHandleBuilder { /* … */ }
```

`TracerHandle::default()` still yields an empty handle, so **every existing
`TracerHandle::default()` call site keeps compiling** (`context.rs`, `tool.rs`,
`core/tests/*`, `runtime-tokio/tests/*`, the macros end-to-end test, and the
`leukemia_classifier` example). The `_private: ()` field is removed — non-breaking,
since external code can't name it.

### 6.2 `Model` trait (`core/src/model.rs`) — additive via default impls

```rust
pub trait Model: Send + Sync {
    async fn invoke(/* … unchanged … */) -> /* … */;
    fn capabilities(&self) -> ModelCapabilities;

    /// GenAI `gen_ai.provider.name` — the provider id (e.g. "openai", "anthropic").
    fn provider(&self) -> &str { "unknown" }
    /// GenAI `gen_ai.request.model` — the configured model id (e.g. "gpt-4o").
    fn model(&self) -> &str { "" }
}
```

Both new methods are object-safe (`&self → &str`). `providers-openai` and
`providers-anthropic` override both; the model id is already stored at construction
(`AnthropicModelBuilder::new(model_id)`, the OpenAI config). `"unknown"`/`""` are
elided from the span so an un-overriding `Model` simply omits the attribute.

## 7. Example app

`crates/paigasus-helikon/examples/langfuse_tracing.rs`:

1. Build an `opentelemetry_sdk` `TracerProvider` with a batch span processor and an
   `opentelemetry-otlp` **HTTP/protobuf** exporter targeting Langfuse's OTLP endpoint
   (`…/api/public/otel/v1/traces`) with a Basic-auth header (base64 `public:secret`)
   from `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_HOST`.
2. Install a small **tags span processor** that, on span end, converts the
   `langfuse.trace.tags` JSON-string attribute into a native `string[]` OTel attribute
   (so `core` stays `tracing`-only — Decision 5).
3. Install `tracing_subscriber::registry().with(tracing_opentelemetry::layer()…)`.
4. Build an `LlmAgent`, construct a `RunContext` with a populated `TracerHandle`
   (`session_id` / `user_id` / `tags`), run via `TokioRunner`.
5. Flush + shut down the provider on exit so the batch processor drains.

New **dev-dependencies on the facade only** (added to `[workspace.dependencies]`,
version-locked to the existing `opentelemetry = "0.27"`):

| Crate | Version | Notes |
|-------|---------|-------|
| `tracing-opentelemetry` | `0.28` | the 0.27-compatible bridge release (verified pairing) |
| `opentelemetry_sdk` | `0.27` | `rt-tokio` feature; provides `InMemorySpanExporter` for §8 |
| `opentelemetry-otlp` | `0.27` | `http-proto` + a TLS-backed `reqwest` (TLS choice resolved in §8.1) |
| `tracing-subscriber` | `0.3` | `env-filter`, `fmt` |

`opentelemetry` (0.27) is already declared in `[workspace.dependencies]` and unused; it
becomes used by the example. No *published* crate gains any of these. (0.27 is aging by
2026 but is the existing pin and dev-only; a one-line follow-up can bump the whole
pinned set together.)

## 8. Verification

* **Primary (CI-runnable):** an in-test `tracing::Layer` captures `(span name, parent,
  fields)`. An integration test drives `LlmAgent` over the existing mock model
  (`core/tests/common`) and asserts: the hierarchy (run → turn → {chat, tool}), the
  `gen_ai.operation.name` values, presence of `gen_ai.provider.name` /
  `gen_ai.request.model` / `gen_ai.agent.name` / `gen_ai.tool.name` / `langfuse.*`, and
  `gen_ai.usage.input_tokens/output_tokens == mock-provider usage`.
* **Higher-fidelity (CI-runnable):** an example/integration test using
  `opentelemetry_sdk::testing::trace::InMemorySpanExporter` + the real
  `tracing-opentelemetry` layer + the tags span processor proves the tracing → OTel
  mapping: dynamic `otel.name` (`chat {model}` etc.), `otel.kind`, typed integer token
  counts, and `langfuse.trace.tags` as a native array.
* **Manual:** "complete trace tree in Langfuse" is inherently manual — documented as a
  runbook in the example's module doc comment.

New public items (`TracerHandle` API, `Model::provider/model`) carry `///` docs to
satisfy `missing_docs` and the 80% doc-coverage gate.

### 8.1 Supply-chain gate (blocking)

Before merge, with the new dev-deps actually added:

1. Run `cargo deny check` and `cargo audit`. Both scan dev-dependencies; the
   reqwest→rustls→`ring` (or `aws-lc-rs`) TLS chain is the expected offender against
   the current license allowlist (`Apache-2.0`, `MIT`, `BSD-2/3`, `ISC`, `MPL-2.0`,
   `Unicode-3.0`, `Zlib`).
2. Resolve findings by **either** pinning `reqwest`'s TLS backend to one whose license
   is already allowlisted, **or** adding a reviewed license clarification/exception in
   `deny.toml` for the specific crate(s). Record the exact resolution in the PR.
3. Confirm `cargo cyclonedx --all-features` on the facade is unaffected (it targets the
   published graph, which excludes dev-deps).

This gate is the proof obligation for Decision 1 — the OTLP dev-deps do enter the
`deny`/`audit` graph and must be verified to pass.

## 9. Risks & open questions

* **Langfuse attribute keys.** Verified against Langfuse's current OTel mapping:
  `langfuse.session.id` and `langfuse.user.id` are correct (scalar strings); tags use
  **`langfuse.trace.tags`** typed `string[]` (the ticket's `langfuse.tags` was stale,
  now corrected). Tags require a native array (Decision 5 / §7).
* **Version lock.** `tracing-opentelemetry 0.28` targets `opentelemetry 0.27`, matching
  `opentelemetry_sdk`/`-otlp 0.27`. Pin exact minors.
* **Supply-chain (deny/audit) — open until §8.1 runs.** The TLS-backend license is the
  live unknown; resolve empirically.
* **Doc reconciliation.** Linear ticket updated; the Notion ADR/page reconciliation is
  drafted from this design.
* **Release ordering.** The facade example uses new `core` API. release-plz must
  publish `core` (feat → minor, **`0.2.3 → 0.3.0`** — version confirmed in
  `[workspace.dependencies]`) before the facade verifies — normal dependency-ordered
  flow handles this (same class as SMA-321). No manual ascend ritual applies (no stub
  ascending; no new crate).

## 10. File-by-file change list

| File | Change |
|------|--------|
| `crates/paigasus-helikon-core/src/context.rs` | Replace `TracerHandle` placeholder with real fields + `TracerHandleBuilder` + accessors; keep `Default`. |
| `crates/paigasus-helikon-core/src/model.rs` | Add `Model::provider()` / `Model::model()` default-impl getters with docs. |
| `crates/paigasus-helikon-core/src/agent.rs` | Open/close the run / turn / model-call spans with semconv `otel.name`/`otel.kind`/`gen_ai.*` fields; thread the turn span into `run_tools_concurrent`; open per-call tool spans via `.instrument`. |
| `crates/paigasus-helikon-providers-openai/src/model.rs` | Override `provider()` → `"openai"`, `model()` → configured id. |
| `crates/paigasus-helikon-providers-anthropic/src/model.rs` | Override `provider()` → `"anthropic"`, `model()` → configured id. |
| `crates/paigasus-helikon/tests/otel_spans.rs` | Add the `tracing::Layer`-capture span-tree integration test (semconv names + token counts). Lives in the facade crate because the OTel exporter is a facade dev-dependency. |
| `crates/paigasus-helikon/examples/langfuse_tracing.rs` | New example: tags span processor + OTLP→Langfuse wiring + run. |
| `crates/paigasus-helikon/Cargo.toml` | Add the four export-stack crates as `[dev-dependencies]`. |
| `Cargo.toml` (root) | Add `tracing-opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-subscriber` to `[workspace.dependencies]`. |
| `deny.toml` (conditional) | Any license exception resolved in §8.1. |
| `docs/superpowers/plans/2026-05-31-sma-322-otel-genai-spans.md` | Implementation plan (next step). |

## 11. Acceptance criteria mapping

| Ticket criterion | Satisfied by |
|------------------|--------------|
| Example exporter produces a complete trace tree in Langfuse | §7 example + tags processor + §8 manual runbook; structure proven by §8 in-memory-exporter test. |
| Token counts on the model-call span match provider usage | §5 — recorded from the loop's existing `latest_usage`; asserted by §8 primary test. |
| "Follows GenAI semconv / works out of the box" | §5 current-semconv names + §1.1 reconciliation; §8 asserts the emitted names/attributes. |

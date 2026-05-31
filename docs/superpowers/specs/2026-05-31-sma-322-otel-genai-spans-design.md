# SMA-322 — OpenTelemetry spans with GenAI semantic conventions

**Status:** Design (approved)
**Issue:** [SMA-322](https://linear.app/smaschek/issue/SMA-322)
**Branch:** `feature/sma-322-opentelemetry-spans-with-genai-semantic-conventions`
**Date:** 2026-05-31
**Milestone:** MVP · **Labels:** `area:observability`, `stage:1`
**References:** Notion *Observability & Evaluation* (`355830e8fbaa81869381f202ca03fee7`); ADR *OpenTelemetry-native observability with GenAI semantic conventions*.

## 1. Summary

Add built-in OpenTelemetry instrumentation to the agent loop following the GenAI
semantic conventions, so Langfuse / Datadog / Jaeger / Tempo / Honeycomb work out of
the box against the *standard* OTLP exporter — no custom exporter code in the SDK.

The library emits **plain `tracing` spans** (zero-cost when no subscriber is
installed — the tokio/hyper convention). The OTel → OTLP → Langfuse export half lives
**entirely in a runnable example** that uses dev-dependencies. Net new *production*
dependency surface is **zero**: `tracing = "0.1"` is already a dependency of `core`,
`providers-openai`, and `providers-anthropic`.

This honors the ticket's "without any custom exporter" goal precisely: because the
spans carry standard GenAI-semconv attribute keys, an off-the-shelf `opentelemetry-otlp`
exporter pointed at any backend ingests them correctly. The SDK's deliverable is the
*instrumentation*; the *wiring* is demonstrated, not shipped as library code.

## 2. Scope

### In scope (the four live execution paths)

```
agent.run            (INTERNAL)
└─ agent.turn        (INTERNAL)
   ├─ gen_ai.chat    (CLIENT)     one per model invocation
   └─ tool.execute   (INTERNAL)   one per concurrent tool call
```

* `tracing` spans for `agent.run`, `agent.turn`, `gen_ai.chat`, `tool.execute`.
* GenAI-semconv attributes: `gen_ai.system`, `gen_ai.request.model`,
  `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `agent.name`, `tool.name`.
* Langfuse-compatible trace-level attributes: `langfuse.session.id`,
  `langfuse.user.id`, `langfuse.tags`, carried on `TracerHandle`.
* A runnable example wiring the Langfuse OTLP exporter.

### Out of scope (deferred to their own tickets)

* **`agent.handoff` span.** The handoff path returns `AgentError::NotImplemented`
  today (`loop_state.rs:499`, `agent.rs:867`). Instrumenting it now would wrap a span
  around a path that only ever errors. Documented as a named seam; wired by the
  handoff ticket.
* **Guardrail / approval spans.** Guardrails are "stored but not driven"
  (`agent.rs:248-251`); approval returns `NotImplemented` (`loop_state.rs:501`). Same
  rationale.
* **A library-side exporter/init helper or a new `-telemetry` crate.** Decided
  against (see §3, Decision 1). The example *is* the wiring.
* **True OTel Baggage cross-service propagation.** See Decision 3.

## 3. Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | **Exporter wiring lives in an example app only** (dev-deps), not in a library helper or new crate. | Keeps the production dep graph and the `deny`/`audit`/`sbom` surface unchanged. The heavy OTLP/gRPC stack never enters a published crate. Standard OTLP "just works" because we follow the convention, so no library helper is required. |
| 2 | **Instrument only the four live paths** (`agent.run` / `agent.turn` / `gen_ai.chat` / `tool.execute`); leave handoff/approval/guardrail as documented seams. | Everything emitted is end-to-end verifiable now; nothing instruments code that returns `NotImplemented`. |
| 3 | **Trace-level metadata is handle-stamped, pure-`tracing`.** `TracerHandle` carries `session_id` / `user_id` / `tags`; every span stamps the `langfuse.*` attributes from it. | Because the handle is threaded `RunContext → ToolContext`, the attributes appear on every span *by construction* — satisfying "propagate to child spans" without OTel Baggage plumbing in `core`. Literal cross-service Baggage is a follow-up. |
| 4 | **`gen_ai.system` / `gen_ai.request.model` via two `Model` trait getters** with default impls. | `core` cannot otherwise learn these — the model id is configured inside each provider and never crosses into `ModelRequest`. Default impls keep external `Model` implementors compiling; `ModelCapabilities` is `Copy`/flags-only and a poor carrier for a runtime `String`. |
| 5 | **`langfuse.tags` recorded as a JSON-array string** for the MVP. | Plain `tracing` cannot emit a native array field, and `core` stays free of `tracing-opentelemetry`. Flagged for verification against live Langfuse (§9). |

## 4. Architecture & seams

All three seams already exist in `core`:

* **`TracerHandle`** — `core/src/context.rs:253`, today a placeholder unit struct
  annotated *"gains real fields with the observability ticket."* This is that ticket.
  Already threaded into `RunContext` (`context.rs:53`) and projected into
  `ToolContext` (`context.rs:143-149`).
* **`LlmAgent::run()`** — `core/src/agent.rs:584`, the `async_stream` that already
  yields `RunStarted → TurnStarted → …deltas… → RunCompleted { usage }`. Spans open
  and close here.
* **`run_tools_concurrent`** — `core/src/agent.rs:488`, where per-call
  `tool.execute` spans wrap each `tool.invoke`.

### 4.1 Span-guard mechanics (the one real implementation subtlety)

An `Entered` guard from `Span::enter()` is `!Send` and must **never** be held across
an `.await` or `yield` — both pervasive in the `async_stream`. The design therefore
**never enters spans**:

* Hold `tracing::Span` **handles** (which are `Send + Sync`) in loop variables across
  turns; close a span by **dropping its handle**.
* Build the hierarchy with **explicit parents**:
  `info_span!(parent: &turn_span, "gen_ai.chat", …)`. Explicit parenting does *not*
  require the parent to be entered.
* Wrap discrete async futures with `.instrument(span)` where it composes cleanly —
  specifically each per-tool `tool.invoke` future inside `run_tools_concurrent`
  (`Instrument` is `Send`-safe and enters/exits around each poll).
* OTel span timing == tracing span lifetime (create → drop). Deferred attributes
  (token counts, status) are declared `tracing::field::Empty` at creation and filled
  with `span.record("…", value)` before the handle is dropped.
* **Dotted field names are first-class in `tracing`**: the macros treat
  `gen_ai.request.model = %m` as a single field named `"gen_ai.request.model"`, and
  `tracing-opentelemetry` maps that name verbatim to the OTel attribute key.

## 5. Span tree & attribute reference

For each span: `otel.kind` is set via `tracing-opentelemetry`'s recognized special
field. Empty/`"unknown"` attribute values are elided.

### `agent.run` — `otel.kind = "internal"`
| Attribute | Source | When |
|-----------|--------|------|
| `agent.name` | active agent name | open |
| `langfuse.session.id` | `TracerHandle::session_id()` | open |
| `langfuse.user.id` | `TracerHandle::user_id()` | open |
| `langfuse.tags` | `TracerHandle::tags()`, JSON-array string | open |
| `gen_ai.usage.input_tokens` / `gen_ai.usage.output_tokens` | aggregate from `RunCompleted { usage }` | close |
| `otel.status_code` (+ `otel.status_message`) | `"ERROR"` on `RunFailed` | close |

### `agent.turn` — `otel.kind = "internal"`
| Attribute | Source | When |
|-----------|--------|------|
| `gen_ai.operation.name` *(optional)* | `"turn"` | open |
| `langfuse.session.id` / `langfuse.user.id` / `langfuse.tags` | re-stamped from `TracerHandle` for child propagation | open |
| turn index (as a span field) | loop turn counter | open |

### `gen_ai.chat` — `otel.kind = "client"` (one per `NextAction::CallModel`)
| Attribute | Source | When |
|-----------|--------|------|
| `gen_ai.system` | `Model::system()` | open |
| `gen_ai.request.model` | `Model::model()` | open |
| `gen_ai.usage.input_tokens` / `gen_ai.usage.output_tokens` | the loop's existing `latest_usage` for this invocation | close |
| `otel.status_code` | `"ERROR"` on model error | close |

> Span name is the literal `gen_ai.chat` per the ticket (not the newer semconv
> `"{operation} {model}"` form). A future refinement may switch to `gen_ai.responses`
> when a provider reports the Responses API via `capabilities().server_managed_state`;
> out of scope here.

### `tool.execute` — `otel.kind = "internal"` (one per call, parented to the turn)
| Attribute | Source | When |
|-----------|--------|------|
| `tool.name` | `ToolCallRequest::name` | open |
| `otel.status_code` (+ `otel.status_message`) | `"ERROR"` when the outcome is `Err` | close |

**Token-count acceptance criterion** is satisfied directly: `gen_ai.chat` records the
per-invocation `latest_usage` already tracked at `agent.rs:638-685`; `agent.run`
records the aggregate from `RunCompleted { usage }`. No new accounting is introduced.

## 6. Public API changes

### 6.1 `TracerHandle` (`core/src/context.rs`) — additive, non-breaking

```rust
/// Carrier for per-run trace-level attributes stamped onto every span.
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
`leukemia_classifier` example). The `_private: ()` field is removed.

### 6.2 `Model` trait (`core/src/model.rs`) — additive via default impls

```rust
pub trait Model: Send + Sync {
    async fn invoke(/* … unchanged … */) -> /* … */;
    fn capabilities(&self) -> ModelCapabilities;

    /// GenAI `gen_ai.system` — the provider identifier (e.g. "openai", "anthropic").
    fn system(&self) -> &str { "unknown" }
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
   read from `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` / `LANGFUSE_HOST` env vars.
2. Install `tracing_subscriber::registry().with(tracing_opentelemetry::layer()…)`.
3. Build an `LlmAgent`, construct a `RunContext` with a populated `TracerHandle`
   (`session_id` / `user_id` / `tags`), run via `TokioRunner`.
4. Flush + shut down the provider on exit so the batch processor drains.

New **dev-dependencies on the facade only** (added to `[workspace.dependencies]`,
version-locked to the existing `opentelemetry = "0.27"`):

| Crate | Version | Notes |
|-------|---------|-------|
| `tracing-opentelemetry` | `0.28` | the 0.27-compatible bridge release |
| `opentelemetry_sdk` | `0.27` | `rt-tokio` feature |
| `opentelemetry-otlp` | `0.27` | `http-proto` + `reqwest` features |
| `tracing-subscriber` | `0.3` | `env-filter`, `fmt` |

`opentelemetry` (0.27) is already declared in `[workspace.dependencies]` and unused;
it becomes used by the example. No published crate gains any of these.

## 8. Verification

* **Primary (CI-runnable):** an in-test `tracing::Layer` captures `(span name,
  parent, fields)`. An integration test drives `LlmAgent` over the existing mock
  model (`core/tests/common`) and asserts: the hierarchy (`run → turn → {chat,
  tool}`), presence of the GenAI/Langfuse attributes, and
  `gen_ai.usage.input_tokens/output_tokens == mock-provider usage`. Light test deps
  (`tracing-subscriber` dev-dep on `core` if not already present).
* **Higher-fidelity (CI-runnable):** an example/integration test using
  `opentelemetry_sdk::testing::trace::InMemorySpanExporter` + the real
  `tracing-opentelemetry` layer proves the tracing → OTel mapping (dotted keys, typed
  integer counts, `otel.kind`/`otel.status_code`) without Langfuse.
* **Manual:** "complete trace tree in Langfuse" is inherently manual — documented as a
  runbook in the example's module doc comment.

Both automated checks run under the standard CI gates (`fmt`, `clippy`, `test`,
`docs`, `doc-coverage`). New public items (`TracerHandle` API, `Model::system/model`)
carry `///` docs to satisfy `missing_docs` and the 80% doc-coverage gate.

## 9. Risks & open questions

* **Langfuse attribute keys.** The ticket names `langfuse.session.id` /
  `langfuse.user.id` / `langfuse.tags`; current Langfuse OTel ingestion may expect the
  `langfuse.trace.*` prefix. Confirm against live Langfuse while building the example;
  if it differs, the example's mapping (not `core`) is the single place to adjust, and
  the `core` field names track whatever Langfuse documents.
* **`langfuse.tags` shape.** JSON-array string for the MVP (Decision 5). If Langfuse
  requires a native array attribute, convert in the example's span processor or revisit
  in a follow-up — `core` stays `tracing`-only.
* **OTel/`tracing-opentelemetry` version lock.** `opentelemetry 0.27` pairs with
  `tracing-opentelemetry 0.28` and `opentelemetry_sdk`/`-otlp 0.27`; a mismatch fails
  to compile. Pin exact minors in `[workspace.dependencies]`.
* **Release ordering.** The facade example uses new `core` API. release-plz must
  publish `core` (feat → minor, `0.2.3 → 0.3.0`) before the facade verifies — normal
  dependency-ordered flow handles this, but it is the same class of issue hit in
  SMA-321. No manual ascend ritual applies (no stub ascending; no new crate).

## 10. File-by-file change list

| File | Change |
|------|--------|
| `crates/paigasus-helikon-core/src/context.rs` | Replace `TracerHandle` placeholder with real fields + `TracerHandleBuilder` + accessors; keep `Default`. |
| `crates/paigasus-helikon-core/src/model.rs` | Add `Model::system()` / `Model::model()` default-impl getters with docs. |
| `crates/paigasus-helikon-core/src/agent.rs` | Open/close `agent.run` / `agent.turn` / `gen_ai.chat` spans in the loop; thread the turn span into `run_tools_concurrent`; open per-call `tool.execute` spans via `.instrument`. |
| `crates/paigasus-helikon-providers-openai/src/model.rs` | Override `system()` → `"openai"`, `model()` → configured id. |
| `crates/paigasus-helikon-providers-anthropic/src/model.rs` | Override `system()` → `"anthropic"`, `model()` → configured id. |
| `crates/paigasus-helikon-core/tests/` | Add the `tracing::Layer`-capture span-tree integration test. |
| `crates/paigasus-helikon/examples/langfuse_tracing.rs` | New example: OTLP→Langfuse wiring + run. |
| `crates/paigasus-helikon/Cargo.toml` | Add the four export-stack crates as `[dev-dependencies]`. |
| `Cargo.toml` (root) | Add `tracing-opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-subscriber` to `[workspace.dependencies]`. |
| `docs/superpowers/plans/2026-05-31-sma-322-otel-genai-spans.md` | Implementation plan (next step). |

## 11. Acceptance criteria mapping

| Ticket criterion | Satisfied by |
|------------------|--------------|
| Example exporter produces a complete trace tree in Langfuse | §7 example + §8 manual runbook; structure proven by §8 in-memory-exporter test. |
| Token counts on `gen_ai.chat` match provider usage | §5 — recorded from the loop's existing `latest_usage`; asserted by §8 primary test. |

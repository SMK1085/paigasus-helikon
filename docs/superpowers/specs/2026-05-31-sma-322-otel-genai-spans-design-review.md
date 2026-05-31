# SMA-322 Design Review — OpenTelemetry spans with GenAI semantic conventions

**Reviews:** [`2026-05-31-sma-322-otel-genai-spans-design.md`](./2026-05-31-sma-322-otel-genai-spans-design.md)
**Reviewer perspective:** staff engineering — fitness against the planned design, the *current* GenAI semconv, and downstream blast radius
**Date:** 2026-05-31
**Verdict:** **Approve with changes.** The hard part — async span mechanics in an `async_stream` without holding `!Send` guards across `.await`/`yield` — is done correctly, the code seams all exist, and "zero new *production* deps" is real. But the ticket's headline promise is *semconv compliance so backends work out of the box*, and the chosen span names + several attribute keys **do not match the current OpenTelemetry GenAI semconv** (they faithfully implement a now-stale Notion/ticket plan). That, plus an incorrect "deny/audit surface unchanged" claim, are the two things to fix before the plan. Two of the spec's own open questions I was able to resolve — one in its favor, one against.

## What this was checked against

- **Linear** [SMA-322](https://linear.app/smaschek/issue/SMA-322) (scope + AC) and **Notion** [Observability & Evaluation](https://www.notion.so/355830e8fbaa81869381f202ca03fee7) (the planned design + ADR reference).
- **Current OTel GenAI semantic conventions** (live, 2026) — spans, agent/framework spans, attribute registry; and **Langfuse's OTel attribute mapping**; and the **`tracing-opentelemetry` ↔ `opentelemetry` version pairing**. Sources listed at the end.
- **Code (ground truth)** — `crates/paigasus-helikon-core/src/{context.rs, model.rs, agent.rs, loop_state.rs}`, both provider crates, root `Cargo.toml`, `deny.toml`. Every load-bearing claim was verified against source.

Severity legend: **H** = high / blocking · **M** = medium · **N** = minor / nit. Each item ends with a concrete **Correction**.

---

## H — High-severity (blocking the "works out of the box" AC)

### H1. The span names and several attribute keys don't match the *current* GenAI semconv

The ticket's purpose is *"following the GenAI semantic conventions, so Langfuse / Datadog / Jaeger / Tempo / Honeycomb work out of the box."* Backends that auto-recognize GenAI telemetry (Datadog LLM Observability ingests the semconv natively; Langfuse maps it) key off the standard span names and `gen_ai.*` attributes. The current semconv (verified live) differs from the spec on four points — all inherited from the Notion page/ticket, which were written against an earlier draft:

1. **`gen_ai.system` was renamed to `gen_ai.provider.name`.** The current registry uses `gen_ai.provider.name` (values like `openai`, `anthropic`, `aws.bedrock`); `gen_ai.system` is the older name. The spec, Notion, and the ticket all use `gen_ai.system`.
2. **Span names should be `{gen_ai.operation.name} {model-or-name}`, not literal dotted strings.** The semconv prescribes `chat {gen_ai.request.model}` (e.g. `chat gpt-4o`), `execute_tool {gen_ai.tool.name}`, `invoke_agent {gen_ai.agent.name}`. The spec uses literal `gen_ai.chat`, `tool.execute`, `agent.run`, `agent.turn`. The spec acknowledges *only* the `gen_ai.chat` deviation ("per the ticket, not the newer `{operation} {model}` form").
3. **`gen_ai.operation.name` is a Required attribute and is missing on the model-call span.** The `gen_ai.chat` attribute table (§5) omits `gen_ai.operation.name` (should be `"chat"`) — i.e. the one span representing the LLM call lacks the required discriminator backends use to classify it as a generation. Conversely, `agent.turn` is given `gen_ai.operation.name = "turn"`, which is not a defined operation value.
4. **Agent/tool attributes lack the `gen_ai.` namespace.** The semconv uses `gen_ai.agent.name` and `gen_ai.tool.name` (plus `gen_ai.tool.call.id`, `gen_ai.tool.type`). The spec uses bare `agent.name` / `tool.name`.

Net effect: spans emitted this way are likely ingested as *generic* spans, not recognized as GenAI generation/tool/agent spans, in exactly the backends the AC names. And because span names and attribute keys become part of users' saved queries/dashboards, changing them *after* shipping is a breaking change — far cheaper to get right now while greenfield.

**Correction.** Align to the current semconv and reconcile the Notion page + ticket (the same stale-docs reconciliation done for SMA-321/ADR-13):
- `gen_ai.system` → **`gen_ai.provider.name`** (optionally also emit `gen_ai.system` during a transition window for older backends).
- Span names → **`chat {model}`** for the model call, **`execute_tool {tool}`** for tools, **`invoke_agent {agent}`** for the run span; keep `agent.turn` as an explicitly *custom* internal span (no semconv equivalent — that's fine, just don't stamp `gen_ai.operation.name` on it).
- Add **`gen_ai.operation.name`** (`chat` / `execute_tool` / `invoke_agent`) as a Required attribute on the respective spans.
- `agent.name` → **`gen_ai.agent.name`**; `tool.name` → **`gen_ai.tool.name`**.
- Keep the `langfuse.*` keys (those are correct — see "Verified OK").

If there's a deliberate reason to keep custom names (e.g. an internal dashboard already keys off them), say so explicitly and drop the "follows GenAI semconv / works out of the box" framing to match.

### H2. "deny/audit/sbom surface unchanged" (Decision 1) is incorrect — `cargo deny` scans dev-dependencies

Decision 1's rationale is *"Keeps the production dep graph and the deny/audit/sbom surface unchanged."* The production-dep half is true (`tracing` is already a dep; the OTLP stack is dev-only). The **deny/audit half is not**: I confirmed `deny.toml` has **no `exclude-dev`** and scopes nothing away from dev-dependencies, and `cargo audit` reads the whole `Cargo.lock`. `cargo deny` and `audit` are **required CI gates** (per `CONTRIBUTING.md` / the rulesets). Adding `opentelemetry-otlp` (with `http-proto` + `reqwest`), `opentelemetry_sdk`, `tracing-opentelemetry`, and `tracing-subscriber` as facade dev-deps pulls a large transitive tree (reqwest → hyper/h2 → and a TLS backend → `ring` or `aws-lc-rs`) into that graph. Concretely:
- **License gate risk:** `ring`'s license is a non-SPDX combined license that routinely trips cargo-deny's allowlist; the current allowlist (`Apache-2.0`, `MIT`, `BSD-2/3`, `ISC`, `MPL-2.0`, `Unicode-3.0`, `Zlib`) does **not** obviously cover it. The reqwest TLS stack is the usual offender.
- **Advisory exposure:** dozens of new transitive crates = a larger surface for future RUSTSEC advisories to fail the daily/PR audit.

**Correction.** Before claiming the surface is unchanged, run `cargo deny check` and `cargo audit` with the new dev-deps actually added. Expect to either add a license clarification/exception for `ring` (or pin reqwest to a TLS backend whose license is already allowlisted), or scope dev-deps out of the license check if cargo-deny's version supports it. Rewrite Decision 1 to "no new *production* deps; dev-deps do enter deny/audit and were verified to pass after allowlist adjustments X/Y." (SBOM is likely unaffected since cargo-cyclonedx targets the published graph, but confirm.)

---

## M — Medium

### M1. `langfuse.tags` as a JSON-array *string* will not register as Langfuse tags

Decision 5 emits `langfuse.tags` as a JSON-array string because plain `tracing` can't emit a native array field. Verified against Langfuse's OTel docs: tags are expected as a **native array attribute** (`langfuse.tags = ["staging","demo"]`). A `tracing` string field maps to a string OTel attribute, so Langfuse will see a JSON *string*, not a tag list — tags silently won't populate. The spec flags this (Decision 5 + §9), and the live docs confirm the concern is real, not hypothetical.

**Correction.** Either (a) set `langfuse.tags` as a real array attribute at the OTel layer in the **example** (e.g. in a span processor, or via `tracing-opentelemetry`'s array-valued field support if the version in use exposes it), keeping `core` `tracing`-only; or (b) accept that tags don't work in the MVP and say so explicitly in the example's runbook, rather than emitting a string that looks like it should work. `session.id`/`user.id` are unaffected (they're scalar strings — see Verified OK).

### M2. Deviation from the planned design: OTel Baggage and `agent.handoff` (reconcile, like ADR-13)

Both the **Linear ticket** and the **Notion page** explicitly require *"OTel Baggage for trace-level attributes that propagate to child spans,"* and Notion adds *"(this is what Langfuse's docs recommend)."* The spec instead uses **handle-stamping** (Decision 3) and defers true Baggage. It also defers the **`agent.handoff`** span that both list in scope.

- The **handoff deferral is well-justified** (the path returns `AgentError::NotImplemented` — verified at `loop_state.rs:499`; instrumenting it would wrap a span around a guaranteed error). Keep it; just reconcile the ticket/Notion scope.
- The **Baggage swap is defensible but is a real deviation.** For single-process tracing, stamping `langfuse.*` on the root span + Langfuse's own attribute propagation achieves the same in-trace result (I confirmed Langfuse propagates `session_id`/`user_id`/`tags` across observations from the root). So the spec is probably fine in practice — but it contradicts an explicit ADR/ticket requirement that cited a Langfuse recommendation.

**Correction.** Do the same doc reconciliation used for SMA-321/ADR-13: update the Notion page + ticket to record that (1) trace-level metadata is handle-stamped in-process for the MVP with true cross-service Baggage as a named follow-up, and (2) `agent.handoff` is deferred to the handoff ticket. Verify the Langfuse "Baggage recommended" note isn't load-bearing for ingestion (my read of the current docs says the `langfuse.*` attributes + propagation suffice, so it isn't — but confirm while building the example).

### M3. Decision 3's "appear on every span by construction" overstates what the tables do

Decision 3 says the `langfuse.*` attributes "appear on every span by construction." But the §5 attribute tables stamp them only on `agent.run` and `agent.turn` — not on `gen_ai.chat` or `tool.execute`. For Langfuse this is fine (trace-level attributes on the root + propagation is the norm), so the *behavior* is okay; the *claim* is just inconsistent with the design tables.

**Correction.** Reword Decision 3 to "stamped on the root (and re-stamped on `agent.turn`); Langfuse propagates them to child observations" — or actually stamp all four spans if you want the literal guarantee. Don't leave the rationale claiming something the tables don't do.

---

## N — Minor / nits

### N1. `opentelemetry 0.27` is ~18 months old by mid-2026

The version pairing is **correct** (verified: `tracing-opentelemetry 0.28` ↔ `opentelemetry 0.27`; tracing-opentelemetry runs one minor ahead). But `opentelemetry` is at a much higher minor by 2026 (tracing-opentelemetry ~0.32+). Since these are **dev-deps only** and `opentelemetry = "0.27"` is already the (unused) workspace pin, staying on 0.27 is acceptable and consistent — just note that the example demonstrates an aging OTel, and a one-line follow-up could bump the whole pinned set when convenient. Not blocking.

### N2. Confirm core's current version for the release-ordering note

§9 plans a core `feat → minor` bump `0.2.3 → 0.3.0`. The **`feat → minor` classification is correct** for adding public API (`TracerHandle` fields/builder, `Model::system`/`model`) — better than the patch framing used in the SMA-346 spec. Just verify the starting version: the SMA-346 review found core at `0.2.1` (heading to `0.2.2`), so confirm it's actually at `0.2.3` when this lands. The mechanism (dependency-ordered publish, no stub ascend) is right.

### N3. Removing `_private: ()` from `TracerHandle` is non-breaking — correct

Verified `TracerHandle` is today `#[derive(Debug, Clone, Default)] struct TracerHandle { _private: () }`. External code can't name `_private`, so it constructs via `default()`; replacing the field with real private fields + a builder keeps `default()` working and breaks no external caller. The spec's "every `TracerHandle::default()` call site keeps compiling" is accurate.

---

## Verified OK (checked, no action — including two of the spec's own open questions, resolved)

- **Langfuse `session.id`/`user.id` keys are correct.** §9 worried they "may expect the `langfuse.trace.*` prefix." Resolved **in the spec's favor**: Langfuse's current OTel mapping uses exactly `langfuse.session.id` → `session_id` and `langfuse.user.id` → `user_id`, and explicitly recommends the `langfuse.*` namespace for manual instrumentation. Keep them. (Only `langfuse.tags` has the array problem — M1.)
- **Version lock is correct.** §9's worry about the `tracing-opentelemetry`/`opentelemetry` pairing resolves cleanly: `tracing-opentelemetry 0.28` targets `opentelemetry 0.27`, matching `opentelemetry_sdk`/`-otlp 0.27`. Pin exact minors as the spec says.
- **The async span mechanics are right** — the hard part of this ticket. Holding `Send + Sync` `tracing::Span` *handles* across `.await`/`yield` (never `Entered` guards), explicit `parent:` parenting without entering, drop-to-close, `.instrument(span)` on the per-tool futures, and `field::Empty` + `span.record(...)` for deferred token/status attributes — all correct, and dotted field names do map verbatim to OTel attribute keys via `tracing-opentelemetry`.
- **All code seams exist as described**: `TracerHandle` placeholder with `_private: ()` (`context.rs`), threaded `RunContext → ToolContext::tracer()`; `Model` trait is lean (no `system`/`model` yet); `ModelCapabilities` has `server_managed_state` and is `Copy`; `ModelRequest` carries no model id (so Decision 4's two getters are the right mechanism); providers store `model_id: String` (trivial getter); `run_tools_concurrent` uses `join_all`/`buffered`; per-invocation `latest_usage` is tracked and aggregated into `RunCompleted { usage }`.
- **Zero new *production* dependencies is true** — `tracing` is already a dep of `core` + both providers; the OTLP stack is facade dev-deps only (but see H2 re: deny/audit).
- **Deferring handoff/guardrail/approval spans is well-justified** — verified those paths are `NotImplemented`/stored-but-not-driven, so instrumenting them now would wrap guaranteed-error or dead code.
- **The `Model::system`/`model` default impls and `TracerHandle` changes are additive/non-breaking**, and object-safe.

---

## Required before writing the plan

1. **H1** — align span names + attribute keys to the *current* GenAI semconv (`gen_ai.provider.name`, `{operation} {model}` span names, required `gen_ai.operation.name`, `gen_ai.agent.name`/`gen_ai.tool.name`) and reconcile the Notion page + ticket; otherwise the "works out of the box" AC won't hold for semconv-aware backends and the keys become a breaking change to fix later.
2. **H2** — actually run `cargo deny`/`cargo audit` with the new dev-deps; fix Decision 1's "surface unchanged" claim and add any license allowlist/`ring` clarification needed.

Recommended alongside: **M1** (make `langfuse.tags` a real array or document it as a non-functional MVP placeholder), **M2** (reconcile the Baggage/`agent.handoff` deviation in the docs). The rest are nits.

## Sources

- OTel GenAI spans (span naming, `gen_ai.operation.name`): https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-spans/
- OTel GenAI agent/framework spans (`invoke_agent`, `gen_ai.agent.name`): https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-agent-spans/
- OTel GenAI attribute registry (`gen_ai.provider.name`, `gen_ai.tool.name`): https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/
- Datadog LLM Observability supports OTel GenAI semconv natively: https://www.datadoghq.com/blog/llm-otel-semantic-convention/
- Langfuse OpenTelemetry attribute mapping (`langfuse.session.id`/`user.id`/`tags`): https://langfuse.com/integrations/native/opentelemetry
- `tracing-opentelemetry` releases/changelog (0.28 ↔ opentelemetry 0.27): https://github.com/tokio-rs/tracing-opentelemetry/releases
- Linear [SMA-322](https://linear.app/smaschek/issue/SMA-322) · Notion [Observability & Evaluation](https://www.notion.so/355830e8fbaa81869381f202ca03fee7)

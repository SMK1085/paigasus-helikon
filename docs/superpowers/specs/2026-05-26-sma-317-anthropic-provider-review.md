# SMA-317 — Anthropic provider design review

**Reviewer:** Claude (staff-engineering review)
**Reviewed:** [`2026-05-26-sma-317-anthropic-provider-design.md`](./2026-05-26-sma-317-anthropic-provider-design.md)
**Date:** 2026-05-26
**Sources cross-checked:** Linear SMA-317, Notion "Model Providers" + ADR-10 (no silent auto-retry), the still-pending SMA-316 spec (upstream dependency), `paigasus-helikon-core::{model, item, agent}` as it stands today, Anthropic's published API behavior.

The spec is the strongest of the three provider/macro specs reviewed so far — it correctly resolves the cross-provider settings question and leans on native Anthropic shapes where they exist. Issues below in descending severity.

## Critical issues

### 1. Extended thinking will 400 at request time — missing beta header

`AnthropicModelBuilder::extended_thinking(ExtendedThinking::Enabled { budget_tokens })` injects `"thinking": {...}` into the request body but does not add the required beta header. Anthropic gated extended thinking behind `anthropic-beta: extended-thinking-2025-02-04` (or the rolled-up version-aware header) until GA, and even after GA some versions require an `anthropic-beta` opt-in.

The spec exposes `.beta(...)` as a manual append knob but does not auto-wire it from `ExtendedThinking::Enabled`. A user who sets only `.extended_thinking(...)` will get a 400 from Anthropic with a header-missing error, and the obvious workaround (`.extended_thinking(...).beta("extended-thinking-...")`) requires reading Anthropic's docs to know the right string.

**Fix**: when `ExtendedThinking::Enabled` is set, the builder appends the canonical beta header automatically. Document the header value in the rustdoc so users on alternative gateway versions can override it.

Same logic likely applies to any other beta-gated feature the spec exposes (currently nothing else, but the principle: a typed builder knob that requires a magic header is a footgun).

### 2. Synthesized tool name `__paigasus_structured_output__` is not reserved

The forced-tool synthesis path uses the literal name `__paigasus_structured_output__`. The SMA-315 macro accepts this as a valid tool name (the regex `[A-Za-z_][A-Za-z0-9_-]*` permits it). A user can write `#[tool(name = "__paigasus_structured_output__")] async fn …` and the synthesizer will quietly emit a duplicate tool entry in the request body. Anthropic may reject the duplicate, or — worse — accept it and conflate the two semantically.

The spec defends against the `tool_choice = Tool { name = "..." }` conflict but not against tool-name collision with a user tool.

**Fix**: pick one of:

- Reserve the name. Reject any user tool named `__paigasus_structured_output__` at `Model::invoke` time with `ModelError::Other`, and (ideally, in SMA-315) reject it at proc-macro expansion time with a clear diagnostic.
- Use a name unlikely to collide: `__paigasus__structured_output__<8-char-uuid-baked-at-crate-build-time>` is overkill, but a `paigasus.structured_output` with the `.` character (which the SMA-315 regex rejects) is closed by construction. The trade-off is reading the name in Anthropic's traces is less clean.

The reservation path is simpler.

### 3. `ContentPart::Reasoning` signature round-trip is promised but unimplementable

The §"Messages" table claims:

> `Reasoning → {type: "thinking", thinking, signature}` when round-tripping a previous response with extended thinking enabled

But `ContentPart::Reasoning` in `item.rs` is `Reasoning { text: String }` — there is no `signature` field. The translator literally has no source for the signature. The §"Out of scope" note acknowledges that capturing signatures from streaming output is out of scope — which means **no path** can produce a `ContentPart::Reasoning` carrying a signature, so the table row's `signature` slot is always empty / synthesized / wrong.

Anthropic rejects unsigned `thinking` blocks on input. So either:

- The translator must **always** drop `ContentPart::Reasoning` on input (with `tracing::warn!`) and the table row's "when round-tripping" branch is dead code today.
- Or `ContentPart::Reasoning` needs to grow `signature: Option<String>` and the streaming SSE parser needs to capture `signature_delta`s into it (currently the spec drops `signature_delta` with `tracing::debug!`).

**Decision needed.** The current spec wording is internally inconsistent — it claims to support signed round-trip in one section and rules out capturing signatures in another. Either:

- **(a)** cleanest scoped fix: state in the table that reasoning is always dropped on input today, remove the conditional, and add a follow-up ticket for signed round-trip; or
- **(b)** cross-cutting fix: add `signature: Option<String>` to `ContentPart::Reasoning` in core (small core-trait change), capture signatures from `signature_delta` events into a `BlockState::Thinking { signature: String }` accumulator that emits a final `Reasoning { text, signature }` on `content_block_stop`, and the caller (LlmAgent) gets the signature for free via `Item::AssistantMessage.content`.

Option (b) is the right long-term shape and isn't expensive — but it crosses crate boundaries, so it belongs in the SMA-317 scope only if you're willing to grow it. Otherwise (a).

### 4. In-stream `error` event mapping is inconsistent with HTTP-error mapping

The HTTP error table maps `overloaded_error` (status 529) → `ModelError::Unavailable`, `rate_limit_error` (429) → `RateLimited`, etc. The streaming table maps all in-stream `error` events to `ModelError` via "default `Transport(error.message)` when no other rule fires."

An in-stream `error` SSE event carries `error.type` (e.g. `overloaded_error`, `api_error`). It should route through the same `error.type` → `ModelError` mapping as the HTTP path, not default to `Transport`. As written, a mid-stream overload triggers `Transport(...)` (which is non-retryable but at the wrong semantic level) instead of `Unavailable` (which is correct).

**Fix**: extract the `error.type` → `ModelError` mapping into a shared helper that both paths call. HTTP path provides the status code for the `(status, type)` rows in the table; stream path provides `None` and the table degrades gracefully (e.g. `rate_limit_error` from a stream still becomes `RateLimited`, just without a `retry-after` header).

### 5. `tool_choice: None` semantic departure breaks prompt caching

The spec says: "`tool_choice: None | omit (Anthropic has no native 'none')`". Anthropic added native `{type: "none"}` to `tool_choice` in late 2024. Verify against current docs — if it's available, use the native form; the omit-tools workaround has three downstream problems:

1. **Prompt-cache breakage**: with `CacheStrategy::Tools`, the cache marker normally lands on the last tool in `tools[]`. If `tool_choice: None` omits `tools` from the body entirely, the cached prefix differs from the prefix when tools are present, so cache hits don't happen on alternating turns.
2. **`CacheStrategy::Tools` + `tool_choice: None` has no place to put the marker**. The spec covers the "zero user tools + structured output" case (marker on synthesized tool) but not "user has 5 tools that got omitted for this turn." Currently undefined; needs an explicit rule.
3. **Model reasoning quality**: the model not knowing tools exist (vs knowing but being told not to use them) is a different cognitive setup. For agent loops that toggle tool availability mid-conversation, this matters.

**Fix**: use native `tool_choice: {type: "none"}` if Anthropic supports it (likely yes by 2026-05). If they still don't, at least document the cache-breakage and tool-availability semantic differences explicitly in the `ToolChoice::None` rustdoc.

## Significant issues

### 6. `prompt_caching` backfill on SMA-316 is missing

SMA-317 adds `prompt_caching: bool` to `ModelCapabilities`. The SMA-316 spec's `KNOWN_MODELS` table for OpenAI doesn't set this flag — and OpenAI does support automatic prompt caching on gpt-4o, gpt-4o-mini, gpt-4.1, and the o-series (since late 2024). When `prompt_caching` lands in core, all SMA-316 entries default to `false`, which is wrong.

**Fix**: SMA-317's implementation must update SMA-316's `KNOWN_MODELS` to set `prompt_caching: true` on the models where OpenAI provides it. This is a one-line edit per row but is easy to miss when the change is in a different crate. Add it to the "Cross-crate changes" section.

### 7. `[ToolResult, UserMessage]` adjacency — consecutive user turns

The spec covers `Item::ToolResult` hoisting into the next user turn and adjacent `ToolResult`s coalescing. But what about `[Item::AssistantMessage(tool_use), Item::ToolResult, Item::UserMessage]`? The hoisting rule produces `[assistant(tool_use), user(tool_result), user(message)]` — two consecutive `user` turns, which Anthropic rejects.

The translator must merge the hoisted `tool_result` into the adjacent `UserMessage` (or vice versa). Spec needs an explicit "coalesce ToolResult into immediately-following UserMessage" rule, and a wiremock fixture exercising it.

### 8. `max_tokens = 4096` default silently truncates Claude 4 outputs

The defaults table says: "`max_output_tokens` → `max_tokens` (Anthropic requires it; default to `4096` when caller leaves it unset)."

Claude 4 Sonnet supports outputs up to 64K (8K with extended thinking baseline). Defaulting to 4096 means a user calling `AnthropicModel::messages("claude-sonnet-4-5")` with a default `ModelSettings` will get truncated at 4K tokens with `FinishReason::Length` and may not know why. The truncation is silent at the API level — the user sees a short response and a `Finish::Length` event but typically isn't watching the stream.

**Fix**: either bump the default to a model-aware value (e.g. lookup in `KNOWN_MODELS` for a `max_output_default`), or pick a higher conservative default (16K-32K). Document the chosen value loudly in `ModelSettings::max_output_tokens` rustdoc so users discover the truncation cause.

### 9. Per-call vs per-instance for `extended_thinking` and `top_k`

Both are baked at `build()` time per the §"Type shape" decision. But `extended_thinking.budget_tokens` is a per-task knob (complex tasks want more budget) and `top_k` is genuinely per-call. The spec defends this with "Per-call overrides require rebuilding the model — cheap, no I/O" — but `AnthropicModel` is held inside `LlmAgent.model: Arc<M>` and isn't easily swapped per-turn.

This is the correct trade-off for SMA-317 — the alternative (extending `ModelSettings` with Anthropic-specific fields) is exactly what the spec rejects. But the **practical limitation** for users with variable-thinking-budget workloads is real. Flag this in the §"Known open questions" so a future ticket (`ModelSettings::provider_extensions: HashMap<String, Value>`?) can address it.

### 10. `tool_use_then_continuation.txt` fixture structure is ambiguous

§"Wire integration tests" lists `tool_use_then_continuation.txt` as the multi-turn acceptance fixture. But each `Model::invoke` is one streaming response — Anthropic doesn't continue past a `tool_use` `stop_reason` within one stream. So what does this fixture contain?

Three possibilities:

1. Two SSE streams concatenated (first ends with `stop_reason: tool_use`; second is a continuation after the test injects a tool_result). Test invokes the model twice.
2. One SSE stream where the model emits text, a tool_use, *more text*, and finishes. Anthropic does support this — text continuation around a tool_use within one turn.
3. A misnamed fixture that actually tests a different pattern.

Pick one and document the structure in the spec. Most likely (1), which means the test exercises sequential model invocations with the tool_result injection between them — that's the actual acceptance criterion. Worth being explicit.

### 11. Cache write minimum (~1024 tokens) for live test

Anthropic's prompt cache won't write for prefixes below the per-model minimum (currently ~1024 tokens for Sonnet, ~2048 for Opus). The `live.rs` cache test "two sequential invocations with identical system+tools assert `cached_input_tokens > 0` on the second `Usage`" will pass only if the system+tools prefix exceeds the minimum.

**Fix**: document the cache-write minimum in `CacheStrategy` rustdoc, and either (a) construct the live-test prefix to comfortably exceed 2048 tokens, or (b) skip the assertion with `tracing::info!` if the prefix is too small to cache.

## Smaller items

- **Mid-conversation `Item::System` loses order.** Anthropic has a single top-level `system` field. Multiple `Item::System` items spread through the conversation all get concatenated and applied globally. Document this loss-of-order behavior in the system-row of the translation table.
- **`anthropic-version` default `"2023-06-01"`.** This is the oldest GA version. Newer fields (extended thinking, computer use, image URL inputs on some models) may interact with version-gating. Either bump the default to a current version (e.g. `"2024-10-22"`) or verify the chosen default supports every feature the spec exposes.
- **`MediaSource::Url` with older Claude 3 models.** URL-form image inputs were added in mid-2024. The capabilities table marks `claude-3-opus-20240229` as `vision: true` but that model may only accept base64. If the user passes a URL image with that model, the request 400s. Verify per-model URL support and document the limitation.
- **`MessageTranslator::synthesized_tool_name: Option<String>` is over-engineered.** The name is a compile-time constant; a `bool synthesizing_output` field suffices. Minor.
- **`AssistantMessage.agent` dropped.** Same as SMA-316; consistent, not a bug.
- **Comma-separated `anthropic-beta` building.** If a user passes `.beta("foo,bar")`, treat as one or two? Document.
- **Capabilities table verification.** The spec explicitly defers this to implementation: "Ids above are illustrative; the implementer MUST cross-check." Good.

## Verdict

This is the cleanest of the three specs reviewed and the architectural decisions are well-justified — particularly the "builder-baked Anthropic-specific knobs" call, which is the right answer to the SMA-316 open question.

The four critical fixes are all small, contained changes:

- **#1 (extended-thinking beta header)** — one line in the builder's `build()` to auto-append the header.
- **#2 (synthesized tool name reservation)** — one synchronous check in `Model::invoke` and (ideally) a SMA-315 follow-up.
- **#3 (`ContentPart::Reasoning` signature consistency)** — decide whether to drop-always or extend the carrier type. Either way, remove the contradiction in the spec text.
- **#4 (in-stream error mapping)** — refactor the error mapper into a shared helper, parameterize on status.

Items 5–8 (`tool_choice::None`, `prompt_caching` backfill, ToolResult/UserMessage adjacency, `max_tokens` default) are pre-merge cleanups; 9–11 are wording or test-setup fixes. The smaller list is land-as-implemented.

Once items 1–4 are settled the spec is ready to enter implementation.

## Sources

- [`docs/superpowers/specs/2026-05-26-sma-317-anthropic-provider-design.md`](./2026-05-26-sma-317-anthropic-provider-design.md)
- [Linear SMA-317](https://linear.app/smaschek/issue/SMA-317/anthropic-provider-messages-streaming-tool-use-prompt-caching)
- [Notion — Model Providers](https://www.notion.so/355830e8fbaa81e4979dfe50ee92d3fa)
- [Notion — ADR-10 No silent auto-retry](https://www.notion.so/355830e8fbaa8197b0c8f004d8b10e56)
- [`docs/superpowers/specs/2026-05-26-sma-316-openai-provider-design.md`](./2026-05-26-sma-316-openai-provider-design.md) (upstream dependency)
- `crates/paigasus-helikon-core/src/{model,item,agent}.rs`

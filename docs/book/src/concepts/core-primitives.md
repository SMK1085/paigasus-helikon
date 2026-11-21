# Core Primitives

The seven object-safe traits — `Model`, `Tool<Ctx>`, `Session`, `Guardrail<Ctx>`, `Hook<Ctx>`, `Agent<Ctx>`, `Runner<Ctx>` — and the concrete carrier types they share. The trait surface lives in the [`paigasus-helikon-core`](https://docs.rs/paigasus-helikon-core) crate, the dependency root every other crate builds on.

These seven were chosen as the minimum viable surface. Other primitives users may expect — `Memory`, `KnowledgeBase`, `Toolset`, `Plugin` — are either compositions of these seven (a "toolset" is just a function returning `Vec<Arc<dyn Tool<Ctx>>>`) or premature.

## The seven traits

Each is object-safe by design, so applications hold heterogeneous registries behind `Arc<dyn _>`.

- **`Model`** — the single canonical async interface to an LLM provider. `async fn invoke(&self, request: ModelRequest, cancel: CancellationToken)` returns a `BoxStream` of `ModelEvent`s. Capability differences (streaming, tools, structured output, vision, …) are surfaced through `ModelCapabilities` rather than split into separate traits. One trait covers OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, Bedrock Converse, and Gemini.
- **`Tool<Ctx>`** — a callable an agent can invoke. Reports `name()`, `description()`, a JSON `schema()`, and an `effect()` (`ToolEffect::ReadOnly` / `Write` / `SideEffect`, used by the permission layer). `async fn invoke(&self, ctx: &ToolContext<Ctx>, args: Value)` returns a `ToolOutput`.
- **`Session`** — conversation persistence as an **append-only event log**, not a flat message list. Three methods: `append`, `events` (optionally `since` a `SequenceId`), and `snapshot` (a `ConversationSnapshot` projection of the log). The event-log shape buys deterministic replay for evals, event-sourced durability, and an audit trail.
- **`Guardrail<Ctx>`** — an input/output safety check that runs in parallel with the agent. `async fn check` returns a `GuardrailVerdict` (`Pass` or `Tripwire`); a tripwire halts the run.
- **`Hook<Ctx>`** — a lifecycle interceptor. `async fn on_event(&self, ctx, event: &HookEvent)` returns a `HookDecision` (`Allow`, `Deny`, `ReplaceInput`, `ReplaceOutput`, `InjectSystemMessage`). Hooks are observation and side effects — distinct from permissions (authorization) and guardrails (content).
- **`Agent<Ctx>`** — one trait for both LLM-driven and workflow agents. Reports `name()` / `description()`; `async fn run(&self, ctx: RunContext<Ctx>, input: AgentInput)` returns a `BoxStream<'static, AgentEvent>`.
- **`Runner<Ctx>`** — the pluggable execution backend (the durability seam). `run` / `run_streamed` drive an agent to a `RunResult` / `RunResultStreaming`, with `resume` / `resume_streamed` defaulting on top. Object-safe: methods take `&dyn Agent<Ctx>`. Runners that add a cancel/deadline boundary must honour one precedence rule: **a genuine terminal event (`RunCompleted`/`RunFailed`/`RunDeferred`/`RunStopped`) beats a late cancel or timeout**, which wins only when it aborted the run in-flight. Core owns that rule rather than each runner re-deriving it — call `effective_interrupt(interrupt, saw_terminal)` with `RunInterrupt::{Cancelled, TimedOut}`, and use `AgentEvent::is_terminal` to compute `saw_terminal`. `RunInterrupt::run_error()` gives the canonical `RunError`, and `::terminal_event()` builds the synthetic terminal frame itself — `::terminal_message()` is the text alone.

## Auxiliary traits

Beyond the seven, `core` ships a few narrower traits that support them:

- **`PermissionPolicy<Ctx>`** — a `canUseTool`-style authorizer returning a `PermissionDecision` (`Allow` / `Deny` / `AskUser` / `Replace`). Sits behind the `PermissionMode` enum and first-class `DenyRule`s.
- **`ApprovalHandler`** — resolves a `PermissionDecision::AskUser` into an `ApprovalOutcome`. Non-generic (it needs no `Ctx`).
- **`Instructions<Ctx>`** — renders the system prompt for a turn. Implemented for `String`, `&'static str`, and any `Fn(&RunContext<Ctx>) -> String`, so an agent's instructions can be static text or dynamic per-run.

## Concrete carrier and implementation types

The traits ship with concrete types that carry data across their boundaries and a handful of ready-to-use implementations:

- **`LlmAgent<Ctx, M, T>`** and its typestate builder — the LLM-driven `Agent`. Construct via `LlmAgent::builder::<Ctx>()`, then `.name(...)`, `.model(...)`, `.instructions(...)`, `.tools(...)`, `.handoffs(...)`, `.output_type::<T>()`, and `.build()`.
- **`MemorySession`** — an in-memory `Session` backed by a `Mutex<Vec<SessionEvent>>`, for tests and ephemeral runs. (For persistent storage, see `paigasus-helikon-sessions-sqlite`.)
- **Workflow agents** — `SequentialAgent` (compose with `.then(...)` / `.then_keyed(...)`), `ParallelAgent`, and `LoopAgent`, each implementing `Agent<Ctx>` so they nest like any other agent.
- **`RunContext<Ctx>`** — the per-run state shared across loop, tools, guardrails, and hooks (user context, `Session` handle, `HookRegistry`, `TracerHandle`, `CancellationToken`). Build with `RunContext::new(user_ctx, session, hooks, tracer, cancel)` (all five arguments explicit) or use `RunContext::ephemeral(user_ctx)` as the shorthand for the all-defaults case (in-memory session, empty hooks, default tracer, fresh cancel token); individual parts can then be overridden with the consuming setters `.with_session(...)`, `.with_hooks(...)`, `.with_tracer(...)`, `.with_cancel(...)`. **`RunConfig`** carries per-run knobs (`max_turns`, `timeout`, `parallel_tool_call_limit`, `max_agent_depth`). **`RunResult<T>`** (and its streaming counterpart `RunResultStreaming`) aggregate the run's `final_output`, `events`, and `TokenUsage`.
- **Event and item carriers** — `AgentEvent` is the unified stream variant emitted by every `Agent` (lifecycle, raw `TokenDelta` / `ReasoningDelta` / `ToolCallDelta`, semantic `MessageOutput` / `ToolCallItem` / `ToolOutputItem`, `HandoffItem`, control signals, and terminal `RunCompleted` / `RunFailed` / `RunDeferred` / `RunStopped`). `Item` / `ContentPart` are the conversation-message carriers; `ModelEvent`, `ModelRequest`, and `ToolDef` cross the model boundary; `SessionEvent` is the log entry a `Session` persists.
- **Provider data** — `ContentPart::ProviderData { provider, call_id, data }` carries opaque data that one provider translator owns, for example a Gemini `thoughtSignature`. A model reports it with `ModelEvent::ProviderData`. The rules are these:
  - `provider` is a wire-format key that one translator owns. It is not the value of `Model::provider()`. Each provider crate that writes provider data exports its key as a `pub const`. The Gemini crate exports `PROVIDER_DATA_KEY` (`"gemini"`). A host can use the key to filter or remove the data.
  - A translator reads only the parts with its own key. It ignores every other part without a warning, and it never fails because of one.
  - The part is valid only inside an `Item::AssistantMessage`. Every translator ignores it in any other item.
  - `call_id: Some(id)` means the data belongs to the tool call with that id in the same model response. That call is an `Item::ToolCall` that follows the message, or a `ContentPart::ToolUse` in the same message. Call ids can repeat across responses.
  - The position of the part in `content` has no meaning. The parts keep their arrival order among themselves. A provider that needs block structure stores the whole block in `data`.
  - `Item::is_provider_data_only()` is true for an assistant message that holds only provider data. Such a message has no text for a reader, and a text consumer can skip it.
  - **Rollback note:** a reader on an earlier core version fails on `provider_data` with "unknown variant". After the first turn that stores this variant, a session store or a Temporal history holds it. Deploy readers and Temporal workflow workers before writers. Do not roll back core below this version on a deployment that stored such a turn.

## Canonical reference

The workspace is published on crates.io, and rustdoc HTML is live on docs.rs at <https://docs.rs/paigasus-helikon-core>. Each trait carries a worked rustdoc example; the published rustdoc is the canonical reference for exact signatures and carrier-type fields.

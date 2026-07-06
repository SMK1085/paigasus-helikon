//! Temporal-backed durable runtime for the Paigasus Helikon AI SDK.
//!
//! A [`Runner`](paigasus_helikon_core::Runner) implementation that makes agent runs durable through
//! [Temporal](https://temporal.io). Each run becomes a workflow, each model invocation and tool call
//! becomes an activity, and the agent loop is reconstructed via deterministic replay of the
//! workflow's history. A crash mid-run resumes from the last completed activity, recovering events
//! and run state automatically.
//!
//! # Quick start
//!
//! Start a local dev server:
//!
//! ```bash
//! temporal server start-dev
//! ```
//!
//! Start a worker that registers your agent and serves its activities on a
//! task queue (`NullModel` stands in for any [`Model`](paigasus_helikon_core::Model)
//! impl — e.g. an OpenAI/Anthropic provider crate's model):
//!
//! ```no_run
//! # struct NullModel;
//! # #[async_trait::async_trait]
//! # impl paigasus_helikon_core::Model for NullModel {
//! #     async fn invoke(
//! #         &self,
//! #         _request: paigasus_helikon_core::ModelRequest,
//! #         _cancel: paigasus_helikon_core::CancellationToken,
//! #     ) -> Result<
//! #         futures_core::stream::BoxStream<
//! #             'static,
//! #             Result<paigasus_helikon_core::ModelEvent, paigasus_helikon_core::ModelError>,
//! #         >,
//! #         paigasus_helikon_core::ModelError,
//! #     > {
//! #         let events: Vec<
//! #             Result<paigasus_helikon_core::ModelEvent, paigasus_helikon_core::ModelError>,
//! #         > = vec![];
//! #         Ok(Box::pin(futures_util::stream::iter(events)))
//! #     }
//! #     fn capabilities(&self) -> paigasus_helikon_core::ModelCapabilities {
//! #         paigasus_helikon_core::ModelCapabilities::default()
//! #     }
//! # }
//! use std::sync::Arc;
//!
//! use paigasus_helikon_core::LlmAgent;
//! use paigasus_helikon_runtime_temporal::worker::TemporalAgentWorker;
//! use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Connect to the Temporal server (`temporal server start-dev` locally).
//!     let target = url::Url::parse("http://localhost:7233")?;
//!     let connection = Connection::connect(ConnectionOptions::new(target).build()).await?;
//!     let client = Client::new(connection, ClientOptions::new("default").build())?;
//!
//!     let agent = Arc::new(
//!         LlmAgent::builder::<()>()
//!             .name("assistant")
//!             .model(NullModel)
//!             .build(),
//!     );
//!
//!     // Registration fails fast if the agent uses hooks, guardrails, or
//!     // handoffs (unsupported in v0 — see below). Serves until shutdown.
//!     TemporalAgentWorker::builder::<()>()
//!         .task_queue("helikon-agents")
//!         .client(client)
//!         .with_ctx(|| ())
//!         .register(agent)?
//!         .build()?
//!         .run()
//!         .await?;
//!     Ok(())
//! }
//! ```
//!
//! Then, from any client process, run the agent durably through
//! [`runner::TemporalRunner`] — the run executes on the worker serving the
//! task queue; client-side, the runner needs the agent definition only for
//! its name and session semantics:
//!
//! ```no_run
//! # struct NullModel;
//! # #[async_trait::async_trait]
//! # impl paigasus_helikon_core::Model for NullModel {
//! #     async fn invoke(
//! #         &self,
//! #         _request: paigasus_helikon_core::ModelRequest,
//! #         _cancel: paigasus_helikon_core::CancellationToken,
//! #     ) -> Result<
//! #         futures_core::stream::BoxStream<
//! #             'static,
//! #             Result<paigasus_helikon_core::ModelEvent, paigasus_helikon_core::ModelError>,
//! #         >,
//! #         paigasus_helikon_core::ModelError,
//! #     > {
//! #         let events: Vec<
//! #             Result<paigasus_helikon_core::ModelEvent, paigasus_helikon_core::ModelError>,
//! #         > = vec![];
//! #         Ok(Box::pin(futures_util::stream::iter(events)))
//! #     }
//! #     fn capabilities(&self) -> paigasus_helikon_core::ModelCapabilities {
//! #         paigasus_helikon_core::ModelCapabilities::default()
//! #     }
//! # }
//! use paigasus_helikon_core::{AgentInput, LlmAgent, RunConfig, RunContext, Runner};
//! use paigasus_helikon_runtime_temporal::runner::{TemporalRunner, TemporalRunnerConfig};
//! use temporalio_client::{Client, ClientOptions, Connection, ConnectionOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let target = url::Url::parse("http://localhost:7233")?;
//!     let connection = Connection::connect(ConnectionOptions::new(target).build()).await?;
//!     let client = Client::new(connection, ClientOptions::new("default").build())?;
//!
//!     let agent = LlmAgent::builder::<()>()
//!         .name("assistant")
//!         .model(NullModel)
//!         .build();
//!
//!     let runner = TemporalRunner::new(client, TemporalRunnerConfig::new("helikon-agents"));
//!     let result = runner
//!         .run(
//!             &agent,
//!             RunContext::ephemeral(()),
//!             AgentInput::from_user_text("Hello!"),
//!             RunConfig::default(),
//!         )
//!         .await?;
//!     println!("{}", result.final_output);
//!     Ok(())
//! }
//! ```
//!
//! # v0 Constraint Set
//!
//! The durable Temporal runtime v0 explicitly does not support:
//!
//! - **Hooks** (`on_turn_started`, `on_model_response`, etc.) — arbitrary async code cannot be
//!   made deterministic inside the workflow. Running hooks in activities is deferred past v0.
//! - **Handoffs** (`NextAction::Handoff`) — the agent loop cannot hand off to another system while
//!   remaining durable. Handoffs are rejected at registration time with a descriptive error.
//! - **Guardrails** (input/output validation) — running guardrail logic inside the deterministic
//!   workflow is not yet supported. Guardrails are rejected at registration time.
//! - **Nested agents** (agent-as-tool) — while the tool call executes and completes, the nested
//!   agent's run is opaque to Temporal's durability (not itself durable). The nested run may
//!   complete but the outer run sees it as a black-box activity result, not as a durable workflow.
//!
//! The first three — hooks, handoffs, and (input/output) guardrails — are detected during agent
//! registration (via [`worker::TemporalAgentWorkerBuilder::register`]) and rejected with a
//! descriptive [`worker::RegistrationError`], failing fast rather than silently skipping
//! unsupported features. Nested agent-as-tool runs are **permitted** — registration succeeds —
//! but the nested run executes non-durably inside its tool activity: a crash-retry of that
//! activity re-executes the entire nested run from scratch.
//!
//! # Worker-Side Posture and Security Boundary
//!
//! The agent's [`RunContext`](paigasus_helikon_core::RunContext) (`Ctx` generic type) is not
//! serializable and does not cross the client→worker boundary. For every tool-call activity, the
//! **worker fabricates a fresh `RunContext::ephemeral(ctx).with_cancel(...)`** from its configured
//! context factory. v0 ships this with **fixed, non-configurable defaults**:
//!
//! - `redact_output = true` — tool output is redacted before it re-enters the conversation, using
//!   secrets sourced from the worker process's own environment (`_API_KEY`/`_TOKEN`/`_SECRET`/
//!   `_PASSWORD`/`_CREDENTIAL`-suffixed variables) plus an empty `extra_secrets` list.
//! - [`PermissionMode::Default`](paigasus_helikon_core::PermissionMode) with no `permission_policy`
//!   installed — permissive by default, except for the always-on built-in destructive guards
//!   (blocking `rm -rf /`/`~`, writes under protected system paths, and writes touching
//!   `.git`/`.ssh`/`.env*`).
//! - No `approval_handler` installed — a destructive guard's `Ask` therefore **degrades to
//!   `Deny`**, since nothing is registered to resolve it.
//! - No deny rules, no allow rules, no custom guard rules — the caller cannot install any of
//!   these on the durable path.
//!
//! **None of the caller's `RunContext` configuration propagates to the worker.** Specifically,
//! the following caller-side fields never cross the client→worker boundary: deny/allow rules, the
//! permission mode and policy, the approval handler, and `extra_secrets`. The worker's fixed v0
//! defaults above are authoritative for every tool call executed in the activity, regardless of
//! what the caller configured on its own `RunContext`.
//!
//! **Temporal history is a persistence boundary.** Tool outputs are redacted *before* they are
//! recorded in the workflow history (Temporal's durable storage), per the fixed `redact_output =
//! true` default above. Treat Temporal history as a permanent external record.
//!
//! Worker-side posture configuration — letting a worker operator configure permission mode,
//! deny/allow rules, an approval handler, or `extra_secrets` for its fabricated `RunContext` — is
//! future work, alongside the serializable-`Ctx`-seed mechanism for finer-grained permission
//! inheritance.
//!
//! # Retry Semantics and Tool Idempotency
//!
//! ## Model errors
//!
//! Per [ADR-10](https://smk1085.github.io/paigasus-helikon/contributing/adrs.html), the runtime
//! never retries model errors (`ModelError` variants). Model failures are **non-retryable** at the
//! activity level and cause the run to fail with [`RunError::Agent(...)`](paigasus_helikon_core::RunError)
//! carrying the provider's error message as a plain string. This differs from `TokioRunner`: the
//! typed `AgentError::Model` variant is **not** reconstructed across the durable boundary — the
//! activity failure payload carries only the message, not the original `ModelError`'s structure.
//!
//! If your application needs model retries, wrap your model in `RetryingModel` (from
//! [`paigasus_helikon_runtime_tokio`](https://docs.rs/paigasus-helikon-runtime-tokio)) before
//! registering it with the worker. That's the sanctioned retry path — retries are an
//! application-layer concern, not a runtime concern.
//!
//! ## Tool errors
//!
//! Tool-level errors never fail the activity: they return as part of the
//! [`ToolCallOutcome`](paigasus_helikon_core::ToolCallOutcome) and are fed back to the model as
//! data (the model can see what went wrong and adapt). Only infrastructure-level panics fail the
//! activity attempt.
//!
//! ## Crash-retry vs error-retry
//!
//! The `model_retry_policy` and `tool_retry_policy` on the worker builder control **crash
//! recovery**, not application-level retries:
//!
//! - **Crash recovery**: If the activity worker crashes or times out mid-execution, Temporal
//!   retries the activity on a live worker. The workflow replays its history deterministically,
//!   returns the recorded result for activities that completed, and retries only the in-flight
//!   activity. This is the crash-resume acceptance criterion.
//! - **Application-level errors**: These are marked non-retryable at the activity level (per
//!   ADR-10 for models, or returned as outcomes for tools) and do not trigger retries.
//!
//! **Tool idempotency under crash-retry:** Because a crashed tool activity retries on a live
//! worker, **tool authors must implement idempotency** if a tool call cannot safely run twice.
//! Set `max_attempts: 1` on the tool's retry policy to opt out of crash-recovery (the run fails
//! instead of resuming), accepting the trade-off: the run is no longer durable against crashes
//! mid-tool-call.
//!
//! # Payload Budget and Conversation Size
//!
//! The durable driver builds every `ModelRequest` from the **full conversation** — including all
//! history since the run started. This means each `call_model` activity payload includes the
//! entire conversation-so-far, causing payload size to grow quadratically with the number of turns.
//! Temporal's default gRPC payload limit is ~2–4 MB, with server-side warnings around 512 KB.
//!
//! **Practical budget for v0:** A single activity payload should stay under **~1.5 MB of JSON**.
//! This typically allows **15–20 turns** with tool outputs averaging **≤50 KB each**. A single
//! tool result larger than ~1.5 MB will fail its activity outright.
//!
//! **Strategies to fit within the budget:**
//! - Bound tool output size via the tool implementation (e.g., truncate summaries, paginate results).
//! - Limit `max_turns` in [`RunConfig`](paigasus_helikon_core::RunConfig) to stay within the turn budget.
//! - Monitor your actual payload sizes in Temporal's Web UI to verify your use case fits.
//!
//! Payload codecs (custom serialization), claim-check blob offloading, and conversation
//! compaction are named follow-up work, not silent gaps.
//!
//! # Upgrade Discipline and Determinism
//!
//! The workflow's deterministic core is [`paigasus_helikon_core::loop_state::transition`], which
//! lives in a separately versioned crate. Replaying an in-flight workflow against a worker with a
//! **different version of `paigasus-helikon-core` or `paigasus-helikon-runtime-temporal`** can
//! cause non-determinism errors (the workflow's replayed decisions don't match the new code's
//! logic).
//!
//! **Operational guidance for v0 (machinery deferred to a future release):**
//!
//! 1. **Drain in-flight runs before redeploying.** Agent runs are typically minutes-to-hours, not
//!    months. Before deploying a new worker version with a bumped core/temporal crate:
//!    - Wait for existing workflows to complete, or
//!    - Use blue-green task queues: point the old worker to `"queue-v1"` and the runner to
//!      `"queue-v2"`, run new workflows on v2 while old ones drain from v1, then decommission v1.
//! 2. **Check the CHANGELOG.** Any release of this crate whose transition behavior changed is
//!    flagged as replay-breaking.
//! 3. **Production path:** [Temporal Worker Versioning (Build IDs)](https://docs.temporal.io/workers#worker-versioning)
//!    is the long-term solution for zero-downtime updates; support is pending in the Rust SDK.
//!
//! # Links
//!
//! - [API reference (docs.rs)](https://docs.rs/paigasus-helikon-runtime-temporal)
//! - [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [Runtimes](https://smk1085.github.io/paigasus-helikon/concepts/runtimes.html)
//! - [Temporal documentation](https://docs.temporal.io)
//! - [Source & issues](https://github.com/SMK1085/paigasus-helikon)

/// Thin Temporal activity layer over the SDK-free driver-facing inner
/// functions, plus the process-local per-agent registry a durable worker
/// resolves by name (never serialized — see [`driver::AgentPlan`]'s docs on
/// why). Private: every externally-relevant type it defines is re-exported
/// or consumed through [`worker`].
mod activities;
/// The pure durable-loop step machine.
///
/// [`driver::DurableDriver`] wraps [`paigasus_helikon_core::transition`] with
/// the bookkeeping (conversation, accumulated events, cumulative usage) a
/// Temporal workflow needs to drive an agent run one activity result at a
/// time, without any Temporal SDK dependency of its own.
pub mod driver;
/// Error types for the Temporal-backed durable runtime.
pub mod error;
/// Wire-format payload types exchanged between the Temporal workflow and its
/// activities.
pub mod payloads;
/// The client-side [`paigasus_helikon_core::Runner`] implementation:
/// [`runner::TemporalRunner`] starts the durable workflow, awaits its outcome
/// (with cooperative cancellation), and mirrors `TokioRunner`'s session
/// semantics at the run boundary.
pub mod runner;
/// Temporal worker construction: builds a [`worker::TemporalAgentWorker`]
/// that serves one or more registered [`paigasus_helikon_core::LlmAgent`]s'
/// activities on a task queue.
pub mod worker;
/// The durable agent-loop workflow driven by a
/// [`crate::worker::TemporalAgentWorker`]. Internal: the public entry points
/// are [`worker::TemporalAgentWorker`] (worker side) and
/// [`runner::TemporalRunner`] (client side).
mod workflow;

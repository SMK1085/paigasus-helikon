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
//! context factory, then applies its configured [`worker::WorkerPosture`].
//!
//! ## Configuring the posture
//!
//! [`worker::WorkerPosture`] groups the nine posture knobs
//! [`RunContext`](paigasus_helikon_core::RunContext) already exposes — permission mode,
//! deny/allow/guard rules, a permission policy, an approval handler, the built-in destructive
//! guards, output redaction, and `extra_secrets` — into one value, set via
//! [`worker::TemporalAgentWorkerBuilder::posture`]. **`WorkerPosture::default()` reproduces the
//! v0 fixed defaults exactly**: [`PermissionMode::Default`](paigasus_helikon_core::PermissionMode)
//! with no `permission_policy` installed (a destructive guard's `Ask` therefore degrades to
//! `Deny` unless an `approval_handler` is also configured), the always-on built-in destructive
//! guards (blocking `rm -rf /`/`~`, writes under protected system paths, and writes touching
//! `.git`/`.ssh`/`.env*`), `redact_output = true` (secrets sourced from the worker process's own
//! environment plus an empty `extra_secrets` list), and no deny/allow/guard rules. **A worker
//! that never calls `.posture(...)` behaves byte-for-byte as before.** Loosen or extend that
//! baseline via `with_permission_mode`, `with_deny_rules`, `with_allow_rules`, `with_guard_rules`,
//! `with_permission_policy`, `with_approval_handler`, `without_default_guards`,
//! `without_output_redaction`, and `with_extra_secrets`.
//!
//! **None of the caller's own `RunContext` configuration propagates to the worker.** The
//! following caller-side posture fields never cross the client→worker boundary, regardless of the
//! worker's posture configuration: deny/allow/guard rules, `default_guards`, `redact_output`, the
//! permission mode and policy, the approval handler, and `extra_secrets`. The worker's configured
//! posture is authoritative for every tool
//! call executed in the activity — a client cannot loosen it by configuring its own `RunContext`
//! differently.
//!
//! ## The request-scoped `Ctx` seed
//!
//! A client may attach an explicit, opt-in seed — [`runner::TemporalRunnerConfig::with_ctx_seed`]
//! — that crosses the client→worker boundary as a plain `serde_json::Value`, recorded in
//! [`payloads::WorkflowInput::ctx_seed`]. On the worker side,
//! [`worker::TemporalAgentWorkerBuilder::with_seeded_ctx`] /
//! [`worker::TemporalAgentWorkerBuilder::try_with_seeded_ctx`] reconstitute the per-run `Ctx` from
//! that seed (a worker that only calls `with_ctx` ignores any seed entirely). This is the
//! mechanism for handing request-scoped caller data — tenant id, user id, auth subject — to the
//! worker, **without** ever serializing posture itself: posture stays worker-static (above); the
//! seed carries data, not policy.
//!
//! **Fail-fast contract.** The seeded factory slot is fallible internally: a
//! [`worker::TemporalAgentWorkerBuilder::try_with_seeded_ctx`] factory that rejects a seed maps to
//! a **non-retryable** activity failure, so a malformed or hostile seed fails the run immediately
//! rather than retry-looping forever (`render_instructions` carries no retry policy of its own) or
//! silently falling back to a default identity under the wrong caller. **Prefer
//! `try_with_seeded_ctx` over the infallible `with_seeded_ctx` whenever the seed drives
//! authorization** — `with_seeded_ctx`'s totality contract requires its closure never panic,
//! which is the wrong tool for rejecting a bad seed.
//!
//! **The seed is recorded in Temporal history.** Keep it small (ids, claims) — never bulk data,
//! and never put secrets in it. History is a permanent persistence boundary, same as tool output
//! (below).
//!
//! ## Per-run authorization without a serializable policy
//!
//! A worker-registered [`PermissionPolicy`](paigasus_helikon_core::PermissionPolicy) is a trait
//! object and stays worker-static — it is never serialized. Because the worker rebuilds `Ctx`
//! from the per-run seed before applying posture, the policy's `check(ctx, tool, args)` can read
//! `ctx.user_ctx()` (the seeded value) and make **request-scoped** decisions — e.g. "tenant `acme`
//! may run `Bash`, others may not" — giving per-run authorization from a static policy plus a
//! dynamic seed, with no policy ever crossing the wire.
//!
//! **The policy is only reachable in some modes.**
//! [`PermissionMode::Bypass`](paigasus_helikon_core::PermissionMode) allows every call before the
//! policy runs, and [`PermissionMode::DontAsk`](paigasus_helikon_core::PermissionMode) denies
//! every call the same way (both short-circuit); a seed-driven per-tenant policy is therefore only
//! consulted under `Default`, `AcceptEdits`, or `Plan`. A worker that combines `Bypass`/`DontAsk`
//! posture with a permission policy makes that policy dead code for tool calls — by design, but
//! easy to combine by accident.
//!
//! **Temporal history is a persistence boundary.** Tool outputs are redacted *before* they are
//! recorded in the workflow history (Temporal's durable storage), when `redact_output = true`
//! (the default). A worker that calls `without_output_redaction()` writes **unredacted** tool
//! output into permanent history — a posture choice made loudly, not a default.
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
//! # Heartbeats
//!
//! [`worker::TemporalAgentWorkerBuilder::heartbeat_interval`] is opt-in (off by default,
//! preserving v0 behavior byte-for-byte when unset). When set, the worker's `call_model` and
//! `invoke_tool` activities emit a liveness `record_heartbeat` on that interval (floored to 1s),
//! and the workflow sets `heartbeat_timeout = 2 × interval` on those two activities'
//! `ActivityOptions` (`render_instructions` never gets one — it is fast and does no network I/O).
//! This **speeds reclamation of a crashed worker's in-flight attempt** well below the activity's
//! full `start_to_close` bound.
//!
//! **Honest caveat — a starved live worker can trip the same timeout.** A `heartbeat_timeout`
//! fires whenever the ticker stops polling, which happens both when the worker crashes (the
//! intended win) *and* when a live worker's async executor is starved by a blocking/CPU-bound
//! tool `invoke` (e.g. a synchronous `std::process::Command`, or heavy compute with no `.await`).
//! In that case Temporal re-dispatches the tool call to another worker, narrowing the double-run
//! window from the activity's full `start_to_close` timeout (default 300s) down to roughly
//! `2 × interval`. **Recommendation: offload blocking or CPU-bound tool work via
//! `tokio::task::spawn_blocking`** so the executor keeps polling the heartbeat ticker. This
//! interacts directly with the **tool idempotency under crash-retry** warning above — enabling
//! heartbeats is a latency/reclamation *tuning* choice with that trade-off, not a free win.
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
//! **The `Ctx` seed adds to this budget too.** When a client sets
//! [`runner::TemporalRunnerConfig::with_ctx_seed`], that seed is serialized into the activity
//! input on **every** `render_instructions` and `invoke_tool` call (once per turn and once per
//! tool call, not once per run) — keep it small (ids, claims), not bulk data.
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
//! **SMA-455 wire additions (additive, not an unqualified "replay-breaking" change).** The
//! worker-posture, `Ctx`-seed, and heartbeat features added a `#[serde(default)] ctx_seed` field
//! to [`payloads::WorkflowInput`] and an optional `heartbeat_timeout` to the model/tool
//! `ActivityOptions`. Both are additive by construction: `#[serde(default)]` means a
//! `WorkflowInput` serialized by an older worker still deserializes on a newer one (the field
//! defaults to `None`), and the changed activity-input tuple only affects **newly-scheduled**
//! `render_instructions`/`invoke_tool` activities — an activity that already completed replays
//! its recorded result from history rather than re-executing against the new code. This is a
//! reasoned, honest claim, not a guarantee proven against every upgrade path; **drain-before-
//! upgrade / blue-green task queues (below) remain the conservative, recommended path** regardless
//! of how additive a given change looks on paper.
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
/// Wire codec for activity inputs: one self-describing envelope payload per
/// activity, decoding both that and the legacy 0.2.x positional shapes.
/// Private — the envelope types never cross the public API boundary.
mod activity_input;
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

# Multi-Agent Patterns

Seven shipped composition primitives let you build systems out of more than
one [`Agent`](./core-primitives.md). They split into three families:

- **Control transfer** — `Handoff`: hand the conversation to another agent and
  let it finish the run. `SwarmAgent` builds on the same mechanism: a pool of
  agents with handoffs auto-injected full-mesh, ending as soon as whichever
  member answers instead of handing off further.
- **Sub-agent invocation** — `AgentAsTool`: call another agent like a tool, get
  its result back, and keep going.
- **Deterministic orchestration** — `SequentialAgent`, `ParallelAgent`,
  `LoopAgent`, `GraphAgent`: drive a fixed topology of sub-agents — in order,
  concurrently, in a bounded loop, or across a dependency-gated DAG.

All of these are in `paigasus-helikon-core` and re-exported through
`paigasus_helikon::core`. `SequentialAgent`, `ParallelAgent`, `LoopAgent`,
`SwarmAgent`, and `GraphAgent` each implement the same `Agent<Ctx>` trait as
`LlmAgent`, so they nest and compose freely — a `SequentialAgent` step can
itself be a `ParallelAgent`, and so on.

## Choosing a pattern

| You want to… | Use | What crosses the boundary |
| --- | --- | --- |
| Route to a specialist and let it own the rest of the run | `Handoff` | The active agent switches; the specialist produces the final output |
| Call a sub-agent, read its answer, and keep reasoning | `AgentAsTool` | The sub-agent's `final_output` comes back as a tool result |
| Run agents in a fixed order, each reading the last | `SequentialAgent` | Each step's final text lands in `state[key]` |
| Fan out independent work concurrently | `ParallelAgent` | Each branch writes `state[key]`; outputs merge into one JSON object |
| Refine until a sub-agent signals "done" | `LoopAgent` | Same as sequential, plus an escalate signal that stops the loop |
| Let a pool of specialists hand off among themselves until one answers | `SwarmAgent` | `HandoffItem` events between members; the first member to answer (not hand off) produces the final output |
| Run a fixed DAG where each node waits on specific predecessors | `GraphAgent` | Each node's final text lands in `state[key]`; sink node(s) deterministically merge into the final output |

`Handoff` is one-way: control does not return to the routing agent.
`AgentAsTool` is request/response: control always returns. The orchestration
agents are deterministic — the topology is fixed in code, not chosen by a model.

## Handoff — transfer control

A `Handoff<Ctx>` names a candidate agent the conversation may be transferred to.
Add handoffs to an `LlmAgent` via `.handoffs(...)`; the agent loop injects one
synthetic `transfer_to_<slug>` tool per handoff, and a model call to one switches
the active agent. The slug lowercases the target's name and collapses
non-alphanumeric runs to `_` (`"investing specialist"` → `transfer_to_investing_specialist`).

Construct with `Handoff::to(agent)` (owned agent) or `Handoff::shared(arc)`
(a pre-wrapped `Arc<dyn Agent<Ctx>>`).

```rust
use paigasus_helikon::core::{
    Agent, AgentInput, Handoff, LlmAgent, RunContext, RunResultStreaming,
};
use paigasus_helikon::openai::OpenAiModel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let budgeting = LlmAgent::builder::<()>()
        .name("budgeting specialist")
        .description("Answers questions about monthly budgets and cutting spending.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are a budgeting specialist. Give concrete, friendly advice.")
        .build();

    let investing = LlmAgent::builder::<()>()
        .name("investing specialist")
        .description("Answers questions about investing, portfolios, and retirement.")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions("You are an investing specialist. Give concrete, prudent advice.")
        .build();

    let triage = LlmAgent::builder::<()>()
        .name("triage")
        .model(OpenAiModel::chat("gpt-5-mini").build()?)
        .instructions(
            "Classify the user's personal-finance question and transfer to the right \
             specialist. Do not answer yourself — always hand off.",
        )
        .handoffs([Handoff::to(budgeting), Handoff::to(investing)])
        .build();

    let ctx: RunContext<()> = RunContext::ephemeral(());

    let input = AgentInput::from_user_text("How should I start investing $5,000?");

    // With handoffs the terminal agent is dynamic, so collect as a string.
    let stream = triage.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream).collect().await?;

    println!("{}", result.final_output);
    Ok(())
}
```

The target's `description` is shown to the model as the transfer tool's
description, so a clear, action-oriented `.description(...)` is what makes routing
work. Two handoffs whose names slug to the same `transfer_to_*` tool are rejected
with a collision error before the first model call, rather than silently
mis-routing.

When a handoff fires, the stream emits a `HandoffItem { from, to }` followed by
`AgentUpdated { agent }`, then the target agent's events. The terminal
`RunCompleted.usage` sums the parent and the post-handoff sub-run. A handoff
chain is bounded by `RunConfig::max_agent_depth`; exceeding it fails the run with
`AgentError::MaxAgentDepthExceeded`.

## AgentAsTool — call a sub-agent and get its result back

`AgentAsTool<Ctx>` adapts any `Agent<Ctx>` into a `Tool<Ctx>`. The parent agent
calls it like any other tool, receives the wrapped agent's `final_output` as a
`ToolOutput`, and keeps reasoning. Construct with `AgentAsTool::new(agent)` or
`AgentAsTool::shared(arc)`; override the exposed name and description with
`.with_name(...)` / `.with_description(...)` (they default to the wrapped agent's
own). The tool advertises a single string argument, `input`.

```rust
use paigasus_helikon::core::{AgentAsTool, LlmAgent};
use paigasus_helikon::openai::OpenAiModel;

let summarizer = LlmAgent::builder::<()>()
    .name("summarizer")
    .description("Condenses a block of text to three bullet points.")
    .model(OpenAiModel::chat("gpt-5-mini").build()?)
    .instructions("Summarize the input as three concise bullets.")
    .build();

let writer = LlmAgent::builder::<()>()
    .name("writer")
    .model(OpenAiModel::chat("gpt-5-mini").build()?)
    .instructions("Draft replies. Call the `summarizer` tool when you need a digest.")
    .tools(vec![std::sync::Arc::new(AgentAsTool::new(summarizer))])
    .build();
```

The sub-run is **isolated**: it gets a fresh in-memory session and empty hooks, so
the wrapped agent's internal turns never touch the parent's session log, and it
runs under its own `RunConfig` (the parent's `max_turns` / `timeout` do not cross
the boundary). What *does* cross: the user context, tracer, cancel token, the
nesting-depth counter (still capped by `max_agent_depth`), and the parent's
**permission** configuration (mode, policy, deny rules, approval handler) — so a
`Plan` or policy decision applies to the wrapped agent's tools too.

Use `AgentAsTool` over `Handoff` when the calling agent must stay in control and
incorporate the sub-agent's answer (a tool the model invokes mid-reasoning),
rather than delegate the whole remaining conversation.

## Orchestration — Sequential / Parallel / Loop

These three drive a fixed list of sub-agents and coordinate them through the
run-scoped `SessionState`: after each sub-agent completes, its final text is
written to `state[key]`, where `key` defaults to the sub-agent's name. A later
step can read it through a dynamic `Instructions` closure. Each merges child event
streams, folds child usage into a running total, and emits one outer
`RunStarted` / `RunCompleted`.

### SequentialAgent

`SequentialAgent::new(name, description)` then chain steps with `.then(agent)`.
Steps run in order and **fail fast** on the first failure. Use
`.then_keyed(key, agent)` when two steps share a name (to avoid a state-key
collision), or `.then_shared(arc)` for a pre-wrapped agent.

```rust
use paigasus_helikon::core::{RunContext, SequentialAgent};

let pipeline = SequentialAgent::new("research-then-write", "Research, then draft.")
    .then(researcher)
    .then(
        LlmAgent::builder::<()>()
            .name("writer")
            .model(OpenAiModel::chat("gpt-5-mini").build()?)
            // Read the previous step's output back out of shared state.
            // `state().get` returns `Option<serde_json::Value>`; the workflow
            // agents store each child's final text as a JSON string, so pull the
            // inner `&str` out rather than `Display`-formatting a quoted `Value`.
            .instructions(|ctx: &RunContext<()>| {
                let notes = ctx
                    .state()
                    .get("researcher")
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                format!("Write a short brief from these notes:\n{notes}")
            })
            .build(),
    );
```

### ParallelAgent

`ParallelAgent::new(name, description)` then add branches with `.add(agent)`,
`.branch(key, agent)`, or `.add_shared(arc)`. Branches run **concurrently** —
cooperatively, via `futures` stream selection rather than OS threads, so it suits
IO-bound `model.invoke` calls; a CPU-bound branch would starve its siblings
between `.await` points. Branch keys must be unique (a duplicate key fails the run
before any branch starts, since concurrent writers to the same key would race).

Failure is **collect-all**: a failed branch lets its siblings finish, then one
aggregate `RunFailed` is emitted. The `final_output` is deterministic — a terminal
`MessageOutput` carrying a sorted-key JSON object `{key: branch_output}`. Read
individual branch results from `state[key]`.

```rust
use paigasus_helikon::core::ParallelAgent;

let fan_out = ParallelAgent::new("multi-lens", "Three independent takes.")
    .branch("optimist", optimist_agent)
    .branch("pessimist", pessimist_agent)
    .branch("realist", realist_agent);
```

### LoopAgent

`LoopAgent::new(name, description, max_iterations)` then add sub-agents with
`.then(agent)`, `.then_keyed(key, agent)`, or `.then_shared(arc)`. It repeats the
sub-agents (in order) up to `max_iterations`. After each sub-agent, the loop
checks its `ActionsHandle`: if a tool **escalated**, the loop emits
`RunCompleted` and stops (success). Escalate means "no more iterations" — the
active sub-agent always finishes its current run first; the check happens after
each sub-agent, so a mid-pass escalate stops before the rest of that pass runs.
Exhausting the budget without an escalate emits `RunFailed` with
`AgentError::MaxIterationsExceeded`.

```rust
use paigasus_helikon::core::LoopAgent;

// Refine up to 5 times: a critic escalates once the draft passes review.
let refine = LoopAgent::new("draft-and-critique", "Iterate until the critic is satisfied.", 5)
    .then(drafter)
    .then(critic);
```

## SwarmAgent — a pool with full-mesh handoffs

`SwarmAgent<Ctx>` wires a pool of `LlmAgent` members together with a
`Handoff` to every other member auto-injected (a `transfer_to_<name>` tool
per pair). Execution is the ordinary handoff-driven `LlmAgent` loop — the
swarm doesn't run a second driver — and it **ends the moment the active
member produces a final output instead of handing off**: first to answer
wins.

Members must be `LlmAgent`s — they're the only agent type that can call
transfer tools (a `dyn Agent` "terminal member" that can receive but never
initiate a handoff is a possible future extension, not shipped today).

**Bounding convergence.** A handoff nests: the parent derives a child run
with its own turn budget and an incremented `agent_depth`, checked against
`RunConfig::max_agent_depth` (default 8). A ping-ponging swarm that never
converges exhausts that depth and fails with
`AgentError::MaxAgentDepthExceeded` — the underlying hard bound. On top of
that, the swarm exposes its own **`.max_handoffs(n)`** budget: it counts
`HandoffItem` events on its own returned stream and, on exceeding `n`,
cancels the run and fails with `AgentError::MaxHandoffsExceeded { limit }`.
Leave it unset and `max_agent_depth` is the only bound.

```rust
use paigasus_helikon::core::SwarmAgent;

let swarm = SwarmAgent::builder()
    .name("support_swarm")
    .description("A personal-finance support swarm covering budgeting and investing.")
    .member(triage)      // LlmAgent<Ctx, M1, T1> — heterogeneous per-member M/T
    .member(budgeting)   // LlmAgent<Ctx, M2, T2>
    .member(investing)
    .entry("triage")     // optional; defaults to the first member added
    .max_handoffs(6)     // optional convergence budget (see above)
    .build()?;           // SwarmBuildError on empty/duplicate-name/unknown-entry
```

The runnable version of this example is
`crates/paigasus-helikon/examples/swarm_finance.rs`.

## GraphAgent — a declared DAG

`GraphAgent<Ctx>` runs a declared directed acyclic graph of agents: nodes are
agents, edges are dependencies, and a node runs only once **all** of its
predecessors have completed. Ready nodes are scheduled and polled
concurrently as their predecessors finish (a dynamic Kahn wavefront) —
more bookkeeping than `ParallelAgent`'s static fan-out, but the same
cooperative, non-tokio concurrency model. Each completed node's final text
is written to `state[key]` (key = the node's name, mirroring
`ParallelAgent`'s branch-key convention) and appended to each successor's
input as a labeled context message.

**Termination** is deterministic regardless of completion timing: once every
node has run, the graph synthesizes one final assistant message from the
**sink** nodes (nodes with no outgoing edges) — a single sink's text goes
through verbatim; multiple sinks merge into a JSON object keyed by node name,
the same convention `ParallelAgent` uses.

**Failure** is collect-all, like `ParallelAgent`: a failed node marks its
transitive descendants as skipped (independent branches still run to
completion), then one aggregate `RunFailed` names both the failed and the
skipped nodes. No new `AgentEvent` variant is added for a skip — it's
surfaced only in that aggregate error.

```rust
use paigasus_helikon::core::GraphAgent;

let graph = GraphAgent::builder()
    .name("finance_report")
    .description("Fans spending and income analysis into a combined summary.")
    .node("spending", spending_agent)   // impl Agent<Ctx> + 'static (or via .shared_node)
    .node("income", income_agent)
    .node("summary", summary_agent)
    .edge("spending", "summary")        // spending → summary
    .edge("income", "summary")          // income → summary
    .build()?;                          // GraphBuildError on cycle/unknown/duplicate/empty
```

The runnable version of this example (a spending/income → summary diamond) is
`crates/paigasus-helikon/examples/graph_report.rs`.

## See also

- [Core Primitives](./core-primitives.md) — the `Agent<Ctx>` trait these all share.
- [The Agent Loop](./agent-loop.md) — how `LlmAgent` drives a single run and its events.
- [Tools](./tools.md) — defining the `Tool<Ctx>` types `AgentAsTool` plugs into.
- [CLI](../reference/cli.md) — the `agents.toml` sidecar's `handoffs` field
  builds one-way `Handoff` chains (cycles rejected at validation, unlike a
  full-mesh `SwarmAgent`).

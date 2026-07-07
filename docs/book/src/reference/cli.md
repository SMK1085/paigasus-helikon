# CLI

`paigasus-helikon-cli` ships two binaries — `helikon` and a `paigasus-helikon`
shim alias for the same command tree — built around a single TOML "sidecar"
file (conventionally `agents.toml`) that declares agents, their tools, and
optional eval configuration. `Ctx = ()` throughout: the CLI is a batteries-included
runner for declarative agents, not a library for embedding custom context types
(for that, depend on the [`paigasus-helikon`](../getting-started/quickstart.md)
facade directly).

## Install

```bash
cargo install paigasus-helikon-cli
```

This installs both binaries. They are identical — `paigasus-helikon` exists
so the crate name and the binary name match (the facade *library* and this
*binary* intentionally share the `paigasus-helikon` name; see the [crate
overview](./crates.md) for why).

```bash
helikon --help
paigasus-helikon --help   # same binary, same command tree
```

## Subcommands

```text
helikon repl      [--agents agents.toml] [--agent NAME]

helikon eval run  <dataset.jsonl> --agent NAME
                  [--agents agents.toml] [--json] [--fail-under F]
                  [--trace sqlite:PATH]

helikon mcp serve --agent NAME
                  [--agents agents.toml] [--http ADDR]
```

`--agents` defaults to `./agents.toml` everywhere, so running from a project
root that contains the sidecar needs no extra flags.

### `helikon repl`

Starts an interactive, hot-reloading REPL against the sidecar.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--agents PATH` | `agents.toml` | The sidecar file to load. |
| `--agent NAME` | alphabetically-first agent | Which agent to start the conversation with. |

### `helikon eval run <dataset>`

Runs a JSONL dataset against one sidecar agent and prints trajectory and
final-response scores.

| Flag | Default | Meaning |
| --- | --- | --- |
| `<dataset>` | *(required, positional)* | Path to the JSONL dataset (one `EvalCase` per line). |
| `--agent NAME` | *(required)* | Which sidecar agent to evaluate. |
| `--agents PATH` | `agents.toml` | The sidecar file to load. |
| `--json` | off | Print the full `EvalReport` as JSON instead of a plain-text table. |
| `--fail-under F` | none | Additionally fail (exit non-zero) if the mean non-skipped score is below `F`. |
| `--trace sqlite:PATH` | none | Record every case into a SQLite trace database at `PATH` (parent directory must exist). |

Exit code is non-zero when any case failed (`EvalReport::passed()` is false)
or, if `--fail-under` is set, when the mean score misses the threshold.

### `helikon mcp serve`

Exposes one sidecar agent as an MCP server.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--agent NAME` | *(required)* | Which sidecar agent to serve. |
| `--agents PATH` | `agents.toml` | The sidecar file to load. |
| `--http ADDR` | none (stdio) | Serve over streamable HTTP at `ADDR` instead of stdio. |

## The `agents.toml` sidecar

One file declares every agent, the tools they can call, and (optionally) how
to evaluate them:

```toml
[agents.triage]
description  = "Routes personal-finance questions"
instructions = "Classify the question and route it to the right specialist."
model        = { provider = "openai", id = "gpt-5-mini" }  # reads OPENAI_API_KEY
tools        = ["lookup_spending"]
handoffs     = ["budgeting"]

[agents.budgeting]
description  = "Answers budgeting questions"
instructions = "Answer using the spending lookup tool when asked about spending."
model        = { provider = "mock", script = "triage_script.json" }
tools        = ["lookup_spending"]

[tools.lookup_spending]
description = "Look up spending for a month"
params      = { type = "object", properties = { month = { type = "string" } }, required = ["month"] }
script      = "tools/lookup_spending.rhai"

[eval]
evaluators = ["exact_match", "tool_trajectory"]
# [eval.json_schema]  schema = "schemas/answer.json"
# [eval.llm_judge]    model = { provider = "openai", id = "gpt-5-mini" }, rubric = "…", threshold = 0.7
```

Key points:

- **`model.provider`** is one of `"openai"`, `"anthropic"`, or `"mock"`. The
  first two resolve `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` from the
  environment at build time and fail fast with a clear error if the key is
  missing. `"mock"` loads a recorded script file (the same `ScriptFile`
  format the `paigasus-helikon-evals` crate uses — see [Observability &
  Evaluation](../concepts/observability-evaluation.md)) — no network, no
  credentials, fully deterministic. **Note:** the `budgeting` agent above
  uses `"mock"` purely to keep this example runnable without an API key;
  swap in `"openai"`/`"anthropic"` for a live agent.
- **`instructions`** is either an inline string (as above) or `{ file =
  "path.md" }`, loaded relative to the sidecar's directory.
- **`tools`** reference `[tools.<name>]` entries by name. Each tool is a
  sandboxed [Rhai](https://rhai.rs) script exposing `fn run(args)`, given
  either as `script = "path.rhai"` or `inline = '''...'''` (exactly one of
  the two). Scripts run under an operation-count cap and have no file or
  network access — a runaway or malicious script fails loudly instead of
  wedging the process.
- **`handoffs`** names other agents in the same file, wired into
  `Handoff::to(...)` chains exactly like the [`Handoff`
  pattern](../concepts/multi-agent-patterns.md#handoff--transfer-control) in
  the core library.
- **`[eval]`** configures `helikon eval run`: `evaluators` names built-in
  evaluators (`exact_match`, `tool_trajectory`, `json_schema`, `llm_judge`);
  `json_schema`/`llm_judge` need their own config block when named. See
  [Observability & Evaluation](../concepts/observability-evaluation.md) for
  what each evaluator scores.

> **Handoff cycles are rejected, not tolerated.** Unlike a full Rust-API
> `SwarmAgent`/`Handoff` graph, the sidecar validates `handoffs` at load time
> with a cycle check: an agent that (transitively) hands off back to itself
> fails to parse with "handoff cycle detected … declare one-way handoff
> chains", rather than being allowed to build (which would create reference
> cycles in a short-lived CLI process). Declare one-way handoff chains in
> `agents.toml`; if you need a true multi-directional pool, use
> [`SwarmAgent`](../concepts/multi-agent-patterns.md#swarmagent--a-pool-with-full-mesh-handoffs)
> from the Rust API instead.

## REPL: slash commands and hot reload

`helikon repl` loads the sidecar into an `AgentRegistry` and watches its
directory for changes (debounced ~300ms). Commands:

| Command | Effect |
| --- | --- |
| `/agents` | List every agent declared in the sidecar, marking the current one. |
| `/switch NAME` | Switch the active agent for the next turn. |
| `/reload` | Force an immediate re-parse of the sidecar file. |
| `/quit` / `/exit` | Leave the REPL. |
| any other text | A conversational turn for the current agent. |

Hot reload happens automatically too — editing and saving the sidecar (or
any file it references) triggers a reload with no `/reload` needed. Two
details matter in practice:

- **The in-flight turn is unaffected.** Each turn builds a fresh agent from
  whatever definitions are current *when the turn starts*; a reload that
  lands mid-turn takes effect starting with the **next** turn, not the one
  in progress.
- **A parse error keeps the old definitions.** If the edited sidecar fails to
  parse or fails validation (bad TOML, dangling tool/handoff reference, a
  handoff cycle), the REPL prints the error and keeps serving the
  last-known-good definitions — a broken save never kills the session.
- **The default agent is the alphabetically first one**, not the first one
  written in the file (agent names are stored in a sorted map internally).
  Pass `--agent NAME` to pick a specific starting agent.

## See also

- [Multi-Agent Patterns](../concepts/multi-agent-patterns.md) — the `Handoff`
  semantics the sidecar's `handoffs` field builds on.
- [Observability & Evaluation](../concepts/observability-evaluation.md) — the
  evals crate that `helikon eval run` and `[eval]` are built on.
- [Crate overview](./crates.md) — `paigasus-helikon-cli`'s published state and
  dependencies.

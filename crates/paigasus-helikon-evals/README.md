# paigasus-helikon-evals

The evaluation harness for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents.

Load a JSONL dataset, run each case through an agent, score the outcome with one or more `Evaluator`s, and collect trajectory-plus-final-response results into a report — deterministically, in CI, via `MockModel` replay or `helikon eval run` (see [`paigasus-helikon-cli`](https://crates.io/crates/paigasus-helikon-cli)).

## Install

```bash
cargo add paigasus-helikon-evals
```

Trace recording is feature-gated and opt-in:

```bash
cargo add paigasus-helikon-evals --features trace-sqlite,trace-parquet
```

## Example

```rust,ignore
use paigasus_helikon_evals::{EvalDataset, EvalRun, ExactMatch, ToolUseTrajectory};

let report = EvalRun::builder()
    .dataset(EvalDataset::from_jsonl_path(std::path::Path::new("triage.jsonl"))?)
    .agent_factory(|case| build_agent_for(&case.id)) // fresh agent (+ MockModel) per case
    .default_ctx()
    .evaluator(ExactMatch::new())
    .evaluator(ToolUseTrajectory::exact())
    .concurrency(4)   // default 1 (sequential); results still come back in dataset order
    .run()
    .await?;

assert!(report.passed());
println!("{}", report.render_table());
```

## Evaluators

| Evaluator | Scores | Skips when |
| --- | --- | --- |
| `ExactMatch` | Trimmed string equality against `expected` (`.case_insensitive()` option); structural JSON equality when `expected` is non-string JSON. | `expected` is absent. |
| `JsonSchemaConformance` | Validates the final output (parsed as JSON) against a constructor-supplied JSON Schema (draft 2020-12). | Never — independent of the case. |
| `LlmJudge` | Wraps an `Arc<dyn Model>` + rubric; scores `{"score": 0..1, "reasoning": "…"}` against a threshold (default `0.7`). | Never — independent of the case. |
| `ToolUseTrajectory` | Compares the observed tool-call sequence to `expected_tools`, `.exact()` or `.in_order()`; `transfer_to_*` handoff calls are filtered out by default. | `expected_tools` is absent. |

`Skipped` is a distinct `ScoreOutcome`, not a failure — it counts toward neither pass/fail nor the summary mean, and `EvalSummary` reports per-evaluator skip counts so a misconfigured dataset shows up rather than silently passing.

## `MockModel` and `ScriptFile`

`MockModel` replays a recorded script (`Vec<ModelEvent>` per `invoke` call) for deterministic, network-free testing. It is stateful — sharing one instance across cases is order-dependent under concurrency — so use `EvalRun::agent_factory` to build a fresh agent (and fresh `MockModel`) per case. `ScriptFile::load` parses a JSON file of serde mirror types (`ScriptEvent`/`ScriptFinishReason`) with a `"default"` script set plus an optional per-case `"cases"` map keyed by case id, so one file can drive a whole dataset.

`MockModel` honors its `CancellationToken` as the `Model::invoke` contract requires: the stream observes the token at each poll and ends on the first fired observation, without emitting `Finish`. The token is observed, not awaited — a consumer that stops polling never learns the stream has ended. An `invoke` called with an already-cancelled token yields an empty stream but still consumes its script, so "one script per `invoke`" holds regardless of cancellation timing.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-evals)
- [Guide: Observability & Evaluation](https://smk1085.github.io/paigasus-helikon/concepts/observability-evaluation.html)
- [Crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.

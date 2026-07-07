//! `helikon eval run`: load a sidecar + JSONL dataset, run every case
//! through the named agent, score it with the sidecar's `[eval]`
//! evaluators, and print a report.

use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use paigasus_helikon_core::{Agent, Model};
use paigasus_helikon_evals::{
    EvalCase, EvalDataset, EvalReport, EvalRun, Evaluator, ExactMatch, JsonSchemaConformance,
    LlmJudge, ScoreOutcome, SqliteTraceSink, ToolUseTrajectory,
};

use crate::cli::EvalRunArgs;
use crate::model::build_model;
use crate::registry::AgentRegistry;
use crate::sidecar::EvalSection;

/// Runs `helikon eval run` and returns the process exit code.
///
/// # Errors
///
/// Returns an error if the sidecar or dataset fails to load, the `[eval]`
/// section is missing/empty, an evaluator's config (schema file, judge
/// model) fails to resolve, the agent fails its pre-flight build (missing
/// or broken tool scripts, instructions files, or mock scripts), or an
/// unknown `--trace` scheme is given.
pub async fn run(args: EvalRunArgs) -> anyhow::Result<ExitCode> {
    let registry = Arc::new(AgentRegistry::load(&args.agents)?);
    let dataset = EvalDataset::from_jsonl_path(&args.dataset)?;
    let base_dir = registry.base_dir();

    let section = registry
        .eval_section()
        .context("sidecar has no [eval] section — add one to run `eval run`")?;
    if section.evaluators.is_empty() {
        anyhow::bail!("[eval] section has no evaluators configured");
    }
    let evaluators = build_evaluators(&section, &base_dir)?;

    // Pre-flight: sidecar validation is filesystem-blind, so a typo'd tool
    // script path, missing instructions file, or malformed mock script only
    // surfaces when the agent is built. Build (and discard) the agent once
    // here — exercising the same file reads as the per-case factory below —
    // so those failures take the CLI's normal error path instead of
    // panicking inside `EvalRun`'s infallible agent factory. The fake case
    // id just selects the mock script file's default scripts.
    registry
        .build_agent_for_case(&args.agent, "__preflight__")
        .with_context(|| {
            format!(
                "agent '{}' failed to build — check tool scripts, instructions files, and mock script paths",
                args.agent
            )
        })?;

    let agent_name = args.agent.clone();
    let factory_registry = Arc::clone(&registry);
    let mut builder = EvalRun::builder::<()>()
        .dataset(dataset)
        .agent_factory(move |case: &EvalCase| {
            Arc::new(
                factory_registry
                    .build_agent_for_case(&agent_name, &case.id)
                    .expect("agent built successfully in pre-flight"),
            ) as Arc<dyn Agent<()>>
        })
        .default_ctx();

    for evaluator in evaluators {
        builder = builder.shared_evaluator(evaluator);
    }

    if let Some(spec) = &args.trace {
        let sink = build_trace_sink(spec).await?;
        builder = builder.trace(sink);
    }

    let report = builder.run().await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render_table());
    }

    let ok = match args.fail_under {
        Some(threshold) => report.passed() && mean_score(&report) >= threshold,
        None => report.passed(),
    };
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Builds the evaluators named in `[eval].evaluators`, resolving any
/// per-evaluator config (schema file, judge model) against `base_dir`.
fn build_evaluators(
    section: &EvalSection,
    base_dir: &Path,
) -> anyhow::Result<Vec<Arc<dyn Evaluator>>> {
    let mut evaluators: Vec<Arc<dyn Evaluator>> = Vec::with_capacity(section.evaluators.len());
    for name in &section.evaluators {
        match name.as_str() {
            "exact_match" => evaluators.push(Arc::new(ExactMatch::new())),
            "tool_trajectory" => evaluators.push(Arc::new(ToolUseTrajectory::exact())),
            "json_schema" => {
                let cfg = section
                    .json_schema
                    .as_ref()
                    .context("evaluator 'json_schema' is declared but its config is missing")?;
                let path = base_dir.join(&cfg.schema);
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read json schema '{}'", path.display()))?;
                let schema: serde_json::Value = serde_json::from_str(&text).with_context(|| {
                    format!("failed to parse json schema '{}' as JSON", path.display())
                })?;
                evaluators.push(Arc::new(JsonSchemaConformance::new(schema)?));
            }
            "llm_judge" => {
                let cfg = section
                    .llm_judge
                    .as_ref()
                    .context("evaluator 'llm_judge' is declared but its config is missing")?;
                let model = build_model(&cfg.model, base_dir)
                    .context("failed to build the llm_judge model")?;
                let mut judge = LlmJudge::new(Arc::new(model) as Arc<dyn Model>);
                if let Some(rubric) = &cfg.rubric {
                    judge = judge.rubric(rubric.clone());
                }
                if let Some(threshold) = cfg.threshold {
                    judge = judge.threshold(threshold);
                }
                evaluators.push(Arc::new(judge));
            }
            // Sidecar validation rejects unknown evaluator names at load
            // time, so this is unreachable in practice.
            other => anyhow::bail!("unknown evaluator '{other}'"),
        }
    }
    Ok(evaluators)
}

/// Parses a `--trace` spec and opens the corresponding sink.
///
/// Only `sqlite:<path>` is currently supported.
async fn build_trace_sink(
    spec: &str,
) -> anyhow::Result<Arc<dyn paigasus_helikon_evals::TraceSink>> {
    match spec.strip_prefix("sqlite:") {
        Some(rest) => {
            let sink = SqliteTraceSink::open(Path::new(rest))
                .await
                .with_context(|| format!("failed to open sqlite trace sink at '{rest}'"))?;
            Ok(Arc::new(sink))
        }
        None => anyhow::bail!("unknown trace scheme in '{spec}' (expected 'sqlite:<path>')"),
    }
}

/// Mean of every non-skipped score value across the whole report.
fn mean_score(report: &EvalReport) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for case in &report.results {
        for evaluator_score in &case.scores {
            if evaluator_score.score.outcome != ScoreOutcome::Skipped {
                sum += evaluator_score.score.value;
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

//! `EvalRun`: orchestrates an evaluation over a dataset and reports the
//! results.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use paigasus_helikon_core::{Agent, AgentInput, RunConfig, RunContext, Runner};

use crate::trace::TraceSink;
use crate::{CaseOutcome, EvalCase, EvalDataset, EvalError, Evaluator, Score, ScoreOutcome};

/// A per-case agent-building closure, boxed to keep [`AgentSource`]'s
/// variant readable.
type AgentFactory<Ctx> = Box<dyn Fn(&EvalCase) -> Arc<dyn Agent<Ctx>> + Send + Sync>;

/// Source of the agent under evaluation: either one shared instance
/// reused across every case, or a factory invoked per case (e.g. to vary
/// agent configuration by case metadata).
enum AgentSource<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// One agent instance, shared (cloned via `Arc`) across every case.
    Shared(Arc<dyn Agent<Ctx>>),
    /// A fresh agent built per case.
    Factory(AgentFactory<Ctx>),
}

impl<Ctx> AgentSource<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn agent_for(&self, case: &EvalCase) -> Arc<dyn Agent<Ctx>> {
        match self {
            Self::Shared(agent) => Arc::clone(agent),
            Self::Factory(f) => f(case),
        }
    }
}

/// Entry point for configuring and running an evaluation.
///
/// Construct via [`EvalRun::builder`].
pub struct EvalRun;

impl EvalRun {
    /// Start building an eval run over context type `Ctx`.
    pub fn builder<Ctx>() -> EvalRunBuilder<Ctx>
    where
        Ctx: Send + Sync + 'static,
    {
        EvalRunBuilder::new()
    }
}

/// Builds an [`EvalRun`]. See the individual setters for what's required
/// vs. optional; [`EvalRunBuilder::run`] validates at call time.
pub struct EvalRunBuilder<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    dataset: Option<EvalDataset>,
    agent: Option<AgentSource<Ctx>>,
    ctx_factory: Option<Arc<dyn Fn() -> Ctx + Send + Sync>>,
    evaluators: Vec<Arc<dyn Evaluator>>,
    concurrency: usize,
    run_config: RunConfig,
    runner: Option<Arc<dyn Runner<Ctx>>>,
    trace: Option<Arc<dyn TraceSink>>,
}

impl<Ctx> EvalRunBuilder<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn new() -> Self {
        Self {
            dataset: None,
            agent: None,
            ctx_factory: None,
            evaluators: Vec::new(),
            concurrency: 1,
            run_config: RunConfig::default(),
            runner: None,
            trace: None,
        }
    }

    /// The dataset to evaluate. Required.
    #[must_use]
    pub fn dataset(mut self, dataset: EvalDataset) -> Self {
        self.dataset = Some(dataset);
        self
    }

    /// One agent instance, shared across every case. Mutually exclusive
    /// with [`EvalRunBuilder::agent_factory`] (the later call wins).
    #[must_use]
    pub fn agent(self, agent: impl Agent<Ctx> + 'static) -> Self {
        self.shared_agent(Arc::new(agent))
    }

    /// As [`EvalRunBuilder::agent`], but takes a pre-built `Arc` to share
    /// one agent instance without an extra layer of indirection.
    #[must_use]
    pub fn shared_agent(mut self, agent: Arc<dyn Agent<Ctx>>) -> Self {
        self.agent = Some(AgentSource::Shared(agent));
        self
    }

    /// Build a fresh agent per case (e.g. to vary configuration by case
    /// metadata). Mutually exclusive with [`EvalRunBuilder::agent`] /
    /// [`EvalRunBuilder::shared_agent`] (the later call wins).
    #[must_use]
    pub fn agent_factory(
        mut self,
        factory: impl Fn(&EvalCase) -> Arc<dyn Agent<Ctx>> + Send + Sync + 'static,
    ) -> Self {
        self.agent = Some(AgentSource::Factory(Box::new(factory)));
        self
    }

    /// Build a fresh `Ctx` per case. Required unless
    /// [`EvalRunBuilder::default_ctx`] is used instead.
    #[must_use]
    pub fn ctx_factory(mut self, factory: impl Fn() -> Ctx + Send + Sync + 'static) -> Self {
        self.ctx_factory = Some(Arc::new(factory));
        self
    }

    /// Use `Ctx::default()` as the context factory.
    #[must_use]
    pub fn default_ctx(self) -> Self
    where
        Ctx: Default,
    {
        self.ctx_factory(Ctx::default)
    }

    /// Add an evaluator, scored against every case's outcome.
    #[must_use]
    pub fn evaluator(self, evaluator: impl Evaluator + 'static) -> Self {
        self.shared_evaluator(Arc::new(evaluator))
    }

    /// As [`EvalRunBuilder::evaluator`], but takes a pre-built `Arc` to
    /// share one evaluator instance.
    #[must_use]
    pub fn shared_evaluator(mut self, evaluator: Arc<dyn Evaluator>) -> Self {
        self.evaluators.push(evaluator);
        self
    }

    /// Number of cases to run concurrently. Clamped to a minimum of 1.
    /// Defaults to `1` (sequential).
    #[must_use]
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Per-run config passed to the runner for every case. Defaults to
    /// [`RunConfig::default`].
    #[must_use]
    pub fn run_config(mut self, run_config: RunConfig) -> Self {
        self.run_config = run_config;
        self
    }

    /// The execution backend to run each case through. Defaults to
    /// `paigasus_helikon_runtime_tokio::TokioRunner`.
    #[must_use]
    pub fn runner(mut self, runner: Arc<dyn Runner<Ctx>>) -> Self {
        self.runner = Some(runner);
        self
    }

    /// A sink that records every case's result once the run completes, in
    /// dataset order (not progressively as cases finish — cases run
    /// concurrently and are re-sorted by original index first).
    #[must_use]
    pub fn trace(mut self, trace: Arc<dyn TraceSink>) -> Self {
        self.trace = Some(trace);
        self
    }

    /// Run every case in the dataset, score it with every evaluator, and
    /// return the aggregated report.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::MissingDataset`], [`EvalError::MissingAgent`],
    /// or [`EvalError::MissingCtxFactory`] if the corresponding builder
    /// step was skipped. Propagates a trace sink's [`crate::TraceError`]
    /// as [`EvalError::Other`].
    pub async fn run(self) -> Result<EvalReport, EvalError> {
        let dataset = self.dataset.ok_or(EvalError::MissingDataset)?;
        let source = Arc::new(self.agent.ok_or(EvalError::MissingAgent)?);
        let ctx_factory = self.ctx_factory.ok_or(EvalError::MissingCtxFactory)?;
        let runner: Arc<dyn Runner<Ctx>> = self
            .runner
            .unwrap_or_else(|| Arc::new(paigasus_helikon_runtime_tokio::TokioRunner));
        let evaluators = Arc::new(self.evaluators);
        let run_config = self.run_config;

        let meta = RunMeta {
            run_id: uuid::Uuid::new_v4().to_string(),
            dataset: dataset.name.clone(),
            started_ts_nanos: jiff::Timestamp::now().as_nanosecond() as i64,
        };

        let mut results: Vec<(usize, CaseResult)> =
            stream::iter(dataset.cases.into_iter().enumerate().map(|(idx, case)| {
                let source = Arc::clone(&source);
                let ctx_factory = Arc::clone(&ctx_factory);
                let runner = Arc::clone(&runner);
                let evaluators = Arc::clone(&evaluators);
                let config = run_config.clone();
                async move {
                    let agent = source.agent_for(&case);
                    let ctx = RunContext::ephemeral((ctx_factory)());
                    let input = AgentInput::from_user_text(case.input.clone());
                    let mut result = CaseResult {
                        case_id: case.id.clone(),
                        input: case.input.clone(),
                        outcome: None,
                        error: None,
                        scores: Vec::new(),
                    };
                    match runner.run(agent.as_ref(), ctx, input, config).await {
                        Err(e) => result.error = Some(e.to_string()),
                        Ok(run_result) => {
                            let outcome = CaseOutcome {
                                final_output: run_result.final_output,
                                events: run_result.events,
                                usage: run_result.usage,
                            };
                            for ev in evaluators.iter() {
                                match ev.evaluate(&case, &outcome).await {
                                    Ok(score) => result.scores.push(EvaluatorScore {
                                        evaluator: ev.name().to_owned(),
                                        score,
                                    }),
                                    Err(e) => result.scores.push(EvaluatorScore {
                                        evaluator: ev.name().to_owned(),
                                        score: Score::failed(0.0, format!("evaluator error: {e}")),
                                    }),
                                }
                            }
                            result.outcome = Some(outcome);
                        }
                    }
                    (idx, result)
                }
            }))
            .buffer_unordered(self.concurrency.max(1))
            .collect()
            .await;
        results.sort_by_key(|(idx, _)| *idx);
        let results: Vec<CaseResult> = results.into_iter().map(|(_, r)| r).collect();

        if let Some(trace) = &self.trace {
            for case in &results {
                trace
                    .record_case(&meta, case)
                    .await
                    .map_err(|e| EvalError::Other(anyhow::Error::new(e)))?;
            }
            trace
                .finish()
                .await
                .map_err(|e| EvalError::Other(anyhow::Error::new(e)))?;
        }

        let summary = summarize(&results);
        Ok(EvalReport {
            meta,
            results,
            summary,
        })
    }
}

/// Run-level metadata recorded once per [`EvalRunBuilder::run`] call.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunMeta {
    /// A fresh v4 UUID identifying this run.
    pub run_id: String,
    /// The dataset's name (see [`EvalDataset::name`]).
    pub dataset: String,
    /// Wall-clock start time, as nanoseconds since the Unix epoch.
    pub started_ts_nanos: i64,
}

/// One evaluator's named score for one case.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvaluatorScore {
    /// The evaluator's [`Evaluator::name`].
    pub evaluator: String,
    /// The score it produced.
    pub score: Score,
}

/// One case's result: its agent-run outcome (or error) plus every
/// evaluator's score.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaseResult {
    /// The case's `id`.
    pub case_id: String,
    /// The case's original input text ([`EvalCase::input`]), recorded
    /// verbatim regardless of whether the agent run succeeded.
    pub input: String,
    /// The run's outcome, `None` if the agent run itself failed.
    pub outcome: Option<CaseOutcome>,
    /// The agent run's error message, if it failed to complete.
    pub error: Option<String>,
    /// Every evaluator's score. Empty when `error` is set — a case whose
    /// agent run failed isn't scored.
    pub scores: Vec<EvaluatorScore>,
}

/// One evaluator's aggregate over every case it scored.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvaluatorSummary {
    /// Mean score value over non-skipped cases (`0.0` if every case was
    /// skipped).
    pub mean: f64,
    /// Number of cases this evaluator passed.
    pub passed: usize,
    /// Number of cases this evaluator failed.
    pub failed: usize,
    /// Number of cases this evaluator skipped (not applicable).
    pub skipped: usize,
}

/// Aggregate statistics over an [`EvalReport`]'s results.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvalSummary {
    /// Per-evaluator summary, keyed by [`Evaluator::name`].
    pub evaluators: BTreeMap<String, EvaluatorSummary>,
    /// Number of cases with no error and no failed evaluator score.
    pub cases_passed: usize,
    /// Number of cases with an agent error or at least one failed score.
    pub cases_failed: usize,
}

/// The full result of an [`EvalRunBuilder::run`] call.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EvalReport {
    /// The run's metadata.
    pub meta: RunMeta,
    /// Every case's result, in dataset order.
    pub results: Vec<CaseResult>,
    /// Aggregate statistics over `results`.
    pub summary: EvalSummary,
}

impl EvalReport {
    /// Whether every case passed (`summary.cases_failed == 0`).
    #[must_use]
    pub fn passed(&self) -> bool {
        self.summary.cases_failed == 0
    }

    /// Render a plain-text summary: one line per case, then a per-evaluator
    /// summary block, then a final `cases: N passed, M failed` line.
    #[must_use]
    pub fn render_table(&self) -> String {
        let mut out = String::new();
        for case in &self.results {
            out.push_str(&case.case_id);
            if let Some(error) = &case.error {
                out.push_str(&format!(" error={error}"));
            }
            for score in &case.scores {
                out.push_str(&format!(
                    " {}={:.3}({:?})",
                    score.evaluator, score.score.value, score.score.outcome
                ));
            }
            out.push('\n');
        }
        for (name, eval_summary) in &self.summary.evaluators {
            out.push_str(&format!(
                "{name}: mean={:.3}, passed={}, failed={}, skipped={}\n",
                eval_summary.mean, eval_summary.passed, eval_summary.failed, eval_summary.skipped
            ));
        }
        out.push_str(&format!(
            "cases: {} passed, {} failed\n",
            self.summary.cases_passed, self.summary.cases_failed
        ));
        out
    }
}

/// Aggregate per-case results into an [`EvalSummary`].
fn summarize(results: &[CaseResult]) -> EvalSummary {
    struct Acc {
        sum: f64,
        scored: usize,
        passed: usize,
        failed: usize,
        skipped: usize,
    }

    let mut by_evaluator: BTreeMap<String, Acc> = BTreeMap::new();
    let mut cases_passed = 0;
    let mut cases_failed = 0;

    for case in results {
        let mut case_ok = case.error.is_none();
        for score in &case.scores {
            let acc = by_evaluator.entry(score.evaluator.clone()).or_insert(Acc {
                sum: 0.0,
                scored: 0,
                passed: 0,
                failed: 0,
                skipped: 0,
            });
            match score.score.outcome {
                ScoreOutcome::Passed => {
                    acc.sum += score.score.value;
                    acc.scored += 1;
                    acc.passed += 1;
                }
                ScoreOutcome::Failed => {
                    acc.sum += score.score.value;
                    acc.scored += 1;
                    acc.failed += 1;
                    case_ok = false;
                }
                ScoreOutcome::Skipped => {
                    acc.skipped += 1;
                }
            }
        }
        if case_ok {
            cases_passed += 1;
        } else {
            cases_failed += 1;
        }
    }

    let evaluators = by_evaluator
        .into_iter()
        .map(|(name, acc)| {
            let mean = if acc.scored == 0 {
                0.0
            } else {
                acc.sum / acc.scored as f64
            };
            (
                name,
                EvaluatorSummary {
                    mean,
                    passed: acc.passed,
                    failed: acc.failed,
                    skipped: acc.skipped,
                },
            )
        })
        .collect();

    EvalSummary {
        evaluators,
        cases_passed,
        cases_failed,
    }
}

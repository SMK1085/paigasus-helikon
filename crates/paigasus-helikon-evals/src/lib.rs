//! Evaluation harness for Paigasus Helikon agents: datasets, evaluators,
//! deterministic replay, and trace recording.
//!
//! The core loop: load an [`EvalDataset`], point an eval run at an
//! agent, attach evaluators, and collect a report of trajectory and
//! final-response scores.

mod dataset;
mod error;
mod evaluator;
mod evaluators;
mod mock;
mod run;
mod script;
mod trace;

pub use dataset::{EvalCase, EvalDataset};
pub use error::EvalError;
pub use evaluator::{CaseOutcome, Evaluator, Score, ScoreOutcome};
pub use evaluators::{ExactMatch, JsonSchemaConformance, LlmJudge, ToolUseTrajectory};
pub use mock::MockModel;
pub use run::{
    CaseResult, EvalReport, EvalRun, EvalRunBuilder, EvalSummary, EvaluatorScore, EvaluatorSummary,
    RunMeta,
};
pub use script::{ScriptEvent, ScriptFile, ScriptFinishReason};
pub use trace::{TraceError, TraceSink};

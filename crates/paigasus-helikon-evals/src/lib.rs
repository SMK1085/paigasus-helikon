//! Evaluation harness for Paigasus Helikon agents: datasets, evaluators,
//! deterministic replay, and trace recording.
//!
//! The core loop: load an [`EvalDataset`], point an eval run at an
//! agent, attach evaluators, and collect a report of trajectory and
//! final-response scores.

mod dataset;
mod error;
mod mock;
mod script;

pub use dataset::{EvalCase, EvalDataset};
pub use error::EvalError;
pub use mock::MockModel;
pub use script::{ScriptEvent, ScriptFile, ScriptFinishReason};

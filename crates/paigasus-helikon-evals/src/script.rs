//! Serde mirror types for recorded model scripts. Core's `ModelEvent`
//! deliberately has no serde; these mirrors keep the file format local
//! to the evals crate (spec §4.3/§6E).

use std::collections::BTreeMap;
use std::path::Path;

use paigasus_helikon_core::{FinishReason, ModelEvent};

use crate::EvalError;

/// Serde mirror of core's `FinishReason`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptFinishReason {
    /// Natural end of turn.
    Stop,
    /// Token limit reached.
    Length,
    /// The model emitted tool calls.
    ToolCalls,
    /// Provider content filter fired.
    ContentFilter,
    /// Any other provider-specific reason.
    Other(String),
}

impl From<ScriptFinishReason> for FinishReason {
    fn from(r: ScriptFinishReason) -> Self {
        match r {
            ScriptFinishReason::Stop => FinishReason::Stop,
            ScriptFinishReason::Length => FinishReason::Length,
            ScriptFinishReason::ToolCalls => FinishReason::ToolCalls,
            ScriptFinishReason::ContentFilter => FinishReason::ContentFilter,
            ScriptFinishReason::Other(s) => FinishReason::Other(s),
        }
    }
}

/// Serde mirror of core's `ModelEvent` (same five variants).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScriptEvent {
    /// Mirror of `ModelEvent::TokenDelta`.
    TokenDelta {
        /// Text chunk.
        text: String,
    },
    /// Mirror of `ModelEvent::ReasoningDelta`.
    ReasoningDelta {
        /// Reasoning text chunk.
        text: String,
    },
    /// Mirror of `ModelEvent::ToolCallDelta`.
    ToolCallDelta {
        /// Provider call id.
        call_id: String,
        /// Tool name (first delta carries it).
        #[serde(default)]
        name: Option<String>,
        /// JSON-arguments fragment.
        args_delta: String,
    },
    /// Mirror of `ModelEvent::Usage`.
    Usage {
        /// Prompt tokens.
        input_tokens: u32,
        /// Completion tokens.
        output_tokens: u32,
        /// Cached prompt tokens, when reported.
        #[serde(default)]
        cached_input_tokens: Option<u32>,
        /// Reasoning tokens, when reported.
        #[serde(default)]
        reasoning_tokens: Option<u32>,
    },
    /// Mirror of `ModelEvent::Finish`.
    Finish {
        /// Why the turn ended.
        reason: ScriptFinishReason,
    },
}

impl From<ScriptEvent> for ModelEvent {
    fn from(e: ScriptEvent) -> Self {
        match e {
            ScriptEvent::TokenDelta { text } => ModelEvent::TokenDelta { text },
            ScriptEvent::ReasoningDelta { text } => ModelEvent::ReasoningDelta { text },
            ScriptEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => ModelEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            },
            ScriptEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                reasoning_tokens,
            } => ModelEvent::Usage {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                reasoning_tokens,
            },
            ScriptEvent::Finish { reason } => ModelEvent::Finish {
                reason: reason.into(),
            },
        }
    }
}

/// A recorded script file: per-invoke scripts, optionally keyed by case id.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ScriptFile {
    /// Scripts used when no case-specific entry matches.
    #[serde(default)]
    pub default: Vec<Vec<ScriptEvent>>,
    /// Case-id-keyed script sets (deterministic multi-case eval).
    #[serde(default)]
    pub cases: BTreeMap<String, Vec<Vec<ScriptEvent>>>,
}

impl ScriptFile {
    /// Load a script file from JSON.
    pub fn load(path: &Path) -> Result<Self, EvalError> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|source| EvalError::Parse { line: 0, source })
    }

    /// Scripts for `case_id` (falling back to `default`), converted to
    /// core `ModelEvent`s.
    pub fn scripts_for(&self, case_id: &str) -> Vec<Vec<ModelEvent>> {
        self.cases
            .get(case_id)
            .unwrap_or(&self.default)
            .iter()
            .map(|script| script.iter().cloned().map(ModelEvent::from).collect())
            .collect()
    }
}

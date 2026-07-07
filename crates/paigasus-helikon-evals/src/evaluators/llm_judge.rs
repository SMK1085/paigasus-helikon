//! LLM-as-judge evaluator: asks a model to rate a run's final output
//! against a rubric.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use paigasus_helikon_core::{AgentInput, CancellationToken, Model, ModelEvent, ModelRequest};

use crate::{CaseOutcome, EvalCase, EvalError, Evaluator, Score, ScoreOutcome};

/// Judges a run's final output with a model call, scored against a
/// rubric.
///
/// The judge model is asked to reply with a single JSON object
/// (`{"score": <0..1>, "reasoning": "..."}`); the response is scanned
/// leniently (first `{` to last `}`) to tolerate surrounding prose. A
/// missing or unparseable JSON object yields a failing `0.0` score
/// rather than an error — judge-model flakiness is a scoring outcome,
/// not a run failure.
pub struct LlmJudge {
    model: Arc<dyn Model>,
    rubric: String,
    threshold: f64,
}

impl LlmJudge {
    /// Judge with `model`. Defaults to a general answer-quality rubric
    /// and a pass threshold of `0.7`.
    pub fn new(model: Arc<dyn Model>) -> Self {
        Self {
            model,
            rubric: "Rate how well the answer addresses the input.".to_owned(),
            threshold: 0.7,
        }
    }

    /// Set the rubric shown to the judge model.
    #[must_use]
    pub fn rubric(mut self, r: impl Into<String>) -> Self {
        self.rubric = r.into();
        self
    }

    /// Set the pass threshold (default `0.7`).
    #[must_use]
    pub fn threshold(mut self, t: f64) -> Self {
        self.threshold = t;
        self
    }
}

#[async_trait]
impl Evaluator for LlmJudge {
    fn name(&self) -> &str {
        "llm_judge"
    }

    async fn evaluate(&self, case: &EvalCase, outcome: &CaseOutcome) -> Result<Score, EvalError> {
        let mut prompt = format!(
            "You are an impartial evaluation judge.\nRubric: {}\n\nInput:\n{}\n",
            self.rubric, case.input
        );
        if let Some(expected) = &case.expected {
            prompt.push_str(&format!("\nReference answer:\n{expected}\n"));
        }
        prompt.push_str(&format!(
            "\nActual answer:\n{}\n\nReply with ONLY a JSON object: {{\"score\": <0..1>, \"reasoning\": \"...\"}}",
            outcome.final_output
        ));

        // `Item::UserMessage` is `#[non_exhaustive]` in core, so it can't be
        // literal-constructed here; `AgentInput::from_user_text` is the
        // portable, published way to build the message vec.
        let mut request = ModelRequest::new();
        request.messages = AgentInput::from_user_text(prompt).messages;

        let mut stream = self
            .model
            .invoke(request, CancellationToken::new())
            .await
            .map_err(|e| EvalError::Run(e.to_string()))?;
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            if let Ok(ModelEvent::TokenDelta { text: t }) = ev {
                text.push_str(&t);
            }
        }

        // Lenient extraction: first '{' … last '}'.
        let json_slice = match (text.find('{'), text.rfind('}')) {
            (Some(a), Some(b)) if b >= a => &text[a..=b],
            _ => {
                return Ok(Score::failed(
                    0.0,
                    format!("judge returned no JSON: {text}"),
                ))
            }
        };

        #[derive(serde::Deserialize)]
        struct Verdict {
            score: f64,
            #[serde(default)]
            reasoning: Option<String>,
        }

        let verdict: Verdict = match serde_json::from_str(json_slice) {
            Ok(v) => v,
            Err(e) => return Ok(Score::failed(0.0, format!("judge JSON parse error: {e}"))),
        };

        let value = verdict.score.clamp(0.0, 1.0);
        if value >= self.threshold {
            Ok(Score {
                value,
                outcome: ScoreOutcome::Passed,
                detail: verdict.reasoning,
            })
        } else {
            Ok(Score {
                value,
                outcome: ScoreOutcome::Failed,
                detail: verdict.reasoning,
            })
        }
    }
}

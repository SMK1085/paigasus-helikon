//! Request and response data-transfer objects (DTOs).
//!
//! These are hand-rolled wire types that sit between HTTP and the core
//! [`paigasus_helikon_core`] types.  Core's [`AgentInput`] / [`AgentEvent`]
//! already derive `Serialize`/`Deserialize`, but they are not directly
//! suitable as HTTP bodies (e.g. `AgentInput` is `#[non_exhaustive]` with
//! constructor-only semantics).  The DTOs here bridge that gap.

use paigasus_helikon_core::{AgentEvent, AgentInput, ContentPart, Item, TokenUsage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── RunRequest ────────────────────────────────────────────────────────────────

/// Internal representation — not public; `RunRequest` wraps it.
enum RunRequestKind {
    Input(String),
    Messages(Vec<Item>),
}

/// HTTP request body for a synchronous or async agent run.
///
/// Accepts **either** of these JSON shapes:
/// - `{ "input": "<text>" }` — shorthand for a single user text message.
/// - `{ "messages": [ … ] }` — an explicit list of [`Item`]s (use when you
///   need multi-turn context or non-text content parts).
///
/// Any other shape is rejected with a deserialization error.
pub struct RunRequest(RunRequestKind);

/// Private untagged helper enum used by `RunRequest`'s custom
/// `Deserialize` implementation.
///
/// `#[serde(untagged)]` tries each variant in order; if both fail
/// (e.g. `{"nope": 1}`), serde returns a descriptive error.
#[derive(Deserialize)]
#[serde(untagged)]
enum RunRequestHelper {
    Input { input: String },
    Messages { messages: Vec<Item> },
}

impl<'de> Deserialize<'de> for RunRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match RunRequestHelper::deserialize(deserializer)? {
            RunRequestHelper::Input { input } => Ok(RunRequest(RunRequestKind::Input(input))),
            RunRequestHelper::Messages { messages } => {
                Ok(RunRequest(RunRequestKind::Messages(messages)))
            }
        }
    }
}

impl RunRequest {
    /// Convert this request into an [`AgentInput`] ready to pass to a runner.
    ///
    /// - The `input` form delegates to [`AgentInput::from_user_text`].
    /// - The `messages` form constructs an `AgentInput` from the supplied
    ///   item list.
    pub fn into_agent_input(self) -> AgentInput {
        match self.0 {
            RunRequestKind::Input(text) => AgentInput::from_user_text(text),
            RunRequestKind::Messages(messages) => {
                let mut input = AgentInput::new();
                input.messages = messages;
                input
            }
        }
    }
}

// ── RunStatus ─────────────────────────────────────────────────────────────────

/// Terminal outcome of an agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum RunStatus {
    /// The run completed normally.
    Completed,
    /// The run finished with an error.
    Failed,
}

// ── RunResponse ───────────────────────────────────────────────────────────────

/// HTTP response body for a completed (non-streaming) agent run.
#[derive(Debug, Clone, Serialize)]
pub struct RunResponse {
    /// Unique identifier for this run, formatted as a UUID string.
    pub run_id: String,
    /// Terminal status of the run.
    pub status: RunStatus,
    /// Concatenated text of the last assistant [`Item::AssistantMessage`]
    /// emitted during the run.  `None` if the run produced no assistant output.
    pub output: Option<String>,
    /// Human-readable error message.  Present only when `status` is
    /// [`RunStatus::Failed`].
    pub error: Option<String>,
    /// Aggregated token usage across the run.  Present only when `status` is
    /// [`RunStatus::Completed`].
    pub usage: Option<TokenUsage>,
    /// All [`AgentEvent`]s emitted during the run, in order.
    pub events: Vec<AgentEvent>,
}

impl RunResponse {
    /// Build a `RunResponse` by scanning `events` for the terminal event and
    /// the last assistant output.
    ///
    /// - The last [`AgentEvent::MessageOutput`] whose inner [`Item`] is an
    ///   [`Item::AssistantMessage`] is used to populate `output` (all
    ///   [`ContentPart::Text`] blocks are concatenated).
    /// - [`AgentEvent::RunCompleted`] sets `status = Completed` and captures
    ///   `usage`.
    /// - [`AgentEvent::RunFailed`] sets `status = Failed` and captures
    ///   `error`.
    /// - If no terminal event is present, `status` defaults to `Failed`
    ///   (defensive).
    pub fn from_events(run_id: Uuid, events: Vec<AgentEvent>) -> Self {
        let mut output: Option<String> = None;
        let mut status = RunStatus::Failed;
        let mut error: Option<String> = None;
        let mut run_usage: Option<TokenUsage> = None;

        for event in &events {
            match event {
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage { content, .. },
                } => {
                    let mut text = String::new();
                    for part in content {
                        if let ContentPart::Text { text: t } = part {
                            text.push_str(t);
                        }
                    }
                    output = Some(text);
                }
                AgentEvent::RunCompleted { usage: u } => {
                    status = RunStatus::Completed;
                    run_usage = Some(*u);
                }
                AgentEvent::RunFailed { error: e } => {
                    status = RunStatus::Failed;
                    error = Some(e.clone());
                }
                _ => {}
            }
        }

        Self {
            run_id: run_id.to_string(),
            status,
            output,
            error,
            usage: run_usage,
            events,
        }
    }
}

// ── AsyncAccepted ─────────────────────────────────────────────────────────────

/// HTTP 202 response body for an accepted asynchronous run.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AsyncAccepted {
    /// Unique identifier for the queued run.
    pub run_id: String,
}

// ── AgentInfo ─────────────────────────────────────────────────────────────────

/// Agent metadata returned by `GET /agents`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AgentInfo {
    /// Machine-readable agent name (matches [`paigasus_helikon_core::Agent::name`]).
    pub name: String,
    /// Human-readable agent description (matches
    /// [`paigasus_helikon_core::Agent::description`]).
    pub description: String,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{AgentEvent, ContentPart, Item, TokenUsage};
    use uuid::Uuid;

    /// Build an [`Item::AssistantMessage`] containing a single text block.
    fn assistant_text(text: &str) -> Item {
        Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            agent: None,
        }
    }

    #[test]
    fn request_accepts_both_forms() {
        let a: RunRequest = serde_json::from_str(r#"{"input":"hi"}"#).unwrap();
        assert_eq!(a.into_agent_input().messages.len(), 1);
        let b: RunRequest = serde_json::from_str(
            r#"{"messages":[{"type":"user_message","content":[{"type":"text","text":"hi"}]}]}"#,
        )
        .unwrap();
        assert_eq!(b.into_agent_input().messages.len(), 1);
        assert!(serde_json::from_str::<RunRequest>(r#"{"nope":1}"#).is_err());
    }

    #[test]
    fn response_from_events_extracts_output_and_usage() {
        let events = vec![
            AgentEvent::MessageOutput {
                item: assistant_text("answer"),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ];
        let r = RunResponse::from_events(Uuid::nil(), events);
        assert_eq!(r.status, RunStatus::Completed);
        assert_eq!(r.output.as_deref(), Some("answer"));
        assert!(r.usage.is_some());
    }
}

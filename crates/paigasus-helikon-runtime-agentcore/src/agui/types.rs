//! AG-UI wire types: the `RunAgentInput` request body and the outbound event frames.
//!
//! AWS passes request payloads to the container without validation, so unknown fields
//! (`tools`, `context`, `state`, `forwardedProps`) are accepted and ignored rather than
//! rejected — compliant AG-UI clients always send them.

use paigasus_helikon_core::{AgentInput, ContentPart, Item};
use serde::Deserialize;

/// AG-UI's `RunAgentInput` request body.
///
/// Only the fields this runtime models are captured; `tools`, `context`, `state` and
/// `forwardedProps` are deliberately absent so serde ignores them (there is no
/// `deny_unknown_fields` here, by design — see the module docs).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunAgentInput {
    /// Client-supplied conversation id. Used only when the platform session header is
    /// absent, and never for persistence — AG-UI mode is stateless per request.
    // Both the AG-UI SSE endpoint (SMA-461 Task 6, src/agui/sse.rs) and the AG-UI
    // WebSocket endpoint (SMA-461 Task 7, src/agui/ws.rs) read this to fall back to a
    // client-supplied thread id when the platform session header is absent.
    #[serde(default)]
    pub(crate) thread_id: Option<String>,
    /// Client-supplied run id, echoed back in `RUN_STARTED`/`RUN_FINISHED`.
    // Both the AG-UI SSE endpoint (SMA-461 Task 6, src/agui/sse.rs) and the AG-UI
    // WebSocket endpoint (SMA-461 Task 7, src/agui/ws.rs) read this and hand it to
    // `EventMapper::new`, which echoes it in `RUN_STARTED`/`RUN_FINISHED`.
    #[serde(default)]
    pub(crate) run_id: Option<String>,
    /// The full conversation. AG-UI clients resend the entire history each request.
    #[serde(default)]
    pub(crate) messages: Vec<AgUiMessage>,
}

/// One entry in `RunAgentInput::messages`.
#[derive(Debug, Deserialize)]
pub(crate) struct AgUiMessage {
    /// Client-assigned message id. Unused by this runtime.
    // No planned caller in this plan reads a client-assigned inbound message id —
    // outbound message ids are generated fresh by the mapper (SMA-461 Task 5).
    // Kept only because the field is part of AG-UI's documented message shape.
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) id: Option<String>,
    /// `"user"`, `"assistant"`, `"system"`, or anything else (ignored).
    pub(crate) role: String,
    /// Message text. A message without content contributes nothing.
    #[serde(default)]
    pub(crate) content: Option<String>,
}

impl RunAgentInput {
    /// Convert the whole conversation into an [`AgentInput`].
    ///
    /// AG-UI mode is stateless per request (the client owns thread state), so *every*
    /// message becomes part of the input rather than only the newest turn.
    // Both the AG-UI SSE endpoint (SMA-461 Task 6, src/agui/sse.rs) and the AG-UI
    // WebSocket endpoint (SMA-461 Task 7, src/agui/ws.rs) call this to seed the agent
    // run from the decoded request body.
    pub(crate) fn into_agent_input(self) -> AgentInput {
        let mut input = AgentInput::new();
        input.messages = self
            .messages
            .into_iter()
            .filter_map(|m| {
                let text = m.content?;
                let content = vec![ContentPart::Text { text }];
                Some(match m.role.as_str() {
                    "assistant" => Item::AssistantMessage {
                        content,
                        agent: None,
                    },
                    "system" => Item::System { content },
                    _ => Item::UserMessage { content },
                })
            })
            .collect();
        input
    }
}

/// Constructors for the outbound AG-UI event frames.
///
/// Frames are `serde_json::Value` because they flow straight into the frame budget,
/// which works on `Value`; a typed enum would be converted right back.
pub(crate) mod event {
    use serde_json::{json, Value};

    /// `RUN_STARTED`.
    // Called by `EventMapper::push` (src/agui/map.rs) on the run's first event.
    pub(crate) fn run_started(thread_id: &str, run_id: &str) -> Value {
        json!({"type": "RUN_STARTED", "threadId": thread_id, "runId": run_id})
    }

    /// `RUN_FINISHED`.
    // Called by `EventMapper::push` (src/agui/map.rs) on the run's terminal event.
    pub(crate) fn run_finished(thread_id: &str, run_id: &str) -> Value {
        json!({"type": "RUN_FINISHED", "threadId": thread_id, "runId": run_id})
    }

    /// `RUN_ERROR`.
    // Called by `EventMapper::push` (src/agui/map.rs) for an in-stream
    // `AGENT_ERROR`; by `error_stream` (SMA-461 Task 6, src/agui/sse.rs) for a
    // pre-stream HTTP error; and directly by the AG-UI WebSocket endpoint (SMA-461
    // Task 7, src/agui/ws.rs) for a malformed inbound frame or a session/context
    // failure, neither of which closes the connection.
    pub(crate) fn run_error(code: &str, message: &str) -> Value {
        json!({"type": "RUN_ERROR", "code": code, "message": message})
    }

    /// `STEP_STARTED`.
    // Called by `EventMapper::push` (src/agui/map.rs) when a turn opens.
    pub(crate) fn step_started(name: &str) -> Value {
        json!({"type": "STEP_STARTED", "stepName": name})
    }

    /// `STEP_FINISHED`.
    // Called by `EventMapper::push`/`finish` (src/agui/map.rs) when a turn closes.
    pub(crate) fn step_finished(name: &str) -> Value {
        json!({"type": "STEP_FINISHED", "stepName": name})
    }

    /// `TEXT_MESSAGE_START`.
    // Called by `EventMapper::push` (src/agui/map.rs) when a text block opens.
    pub(crate) fn text_message_start(message_id: &str) -> Value {
        json!({"type": "TEXT_MESSAGE_START", "messageId": message_id, "role": "assistant"})
    }

    /// `TEXT_MESSAGE_CONTENT`.
    // Called by `EventMapper::push` (src/agui/map.rs) per text delta.
    pub(crate) fn text_message_content(message_id: &str, delta: &str) -> Value {
        json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": message_id, "delta": delta})
    }

    /// `TEXT_MESSAGE_END`.
    // Called by `EventMapper::push`/`finish` (src/agui/map.rs) when a text block
    // closes.
    pub(crate) fn text_message_end(message_id: &str) -> Value {
        json!({"type": "TEXT_MESSAGE_END", "messageId": message_id})
    }

    /// `THINKING_TEXT_MESSAGE_START`.
    // Called by `EventMapper::push` (src/agui/map.rs) when a thinking block opens.
    pub(crate) fn thinking_start(message_id: &str) -> Value {
        json!({"type": "THINKING_TEXT_MESSAGE_START", "messageId": message_id})
    }

    /// `THINKING_TEXT_MESSAGE_CONTENT`.
    // Called by `EventMapper::push` (src/agui/map.rs) per thinking delta.
    pub(crate) fn thinking_content(message_id: &str, delta: &str) -> Value {
        json!({"type": "THINKING_TEXT_MESSAGE_CONTENT", "messageId": message_id, "delta": delta})
    }

    /// `THINKING_TEXT_MESSAGE_END`.
    // Called by `EventMapper::push`/`finish` (src/agui/map.rs) when a thinking
    // block closes.
    pub(crate) fn thinking_end(message_id: &str) -> Value {
        json!({"type": "THINKING_TEXT_MESSAGE_END", "messageId": message_id})
    }

    /// `TOOL_CALL_START`.
    // Called by `EventMapper::push` (src/agui/map.rs) on the first delta for a
    // call id (or, absent any deltas, when a `ToolCallItem` synthesizes the
    // whole triple).
    pub(crate) fn tool_call_start(call_id: &str, name: &str, parent: &str) -> Value {
        json!({
            "type": "TOOL_CALL_START",
            "toolCallId": call_id,
            "toolCallName": name,
            "parentMessageId": parent,
        })
    }

    /// `TOOL_CALL_ARGS`.
    // Called by `EventMapper::push` (src/agui/map.rs) per tool-call argument
    // delta.
    pub(crate) fn tool_call_args(call_id: &str, delta: &str) -> Value {
        json!({"type": "TOOL_CALL_ARGS", "toolCallId": call_id, "delta": delta})
    }

    /// `TOOL_CALL_END`.
    // Called by `EventMapper::push` (src/agui/map.rs) once a tool-call item is
    // materialized.
    pub(crate) fn tool_call_end(call_id: &str) -> Value {
        json!({"type": "TOOL_CALL_END", "toolCallId": call_id})
    }

    /// `TOOL_CALL_RESULT`.
    // Called by `EventMapper::push` (src/agui/map.rs) on a `ToolResult` item.
    pub(crate) fn tool_call_result(call_id: &str, content: &str) -> Value {
        json!({"type": "TOOL_CALL_RESULT", "toolCallId": call_id, "content": content})
    }

    /// `CUSTOM` — the escape hatch for Helikon events AG-UI has no native type for.
    // Called by `EventMapper::push` (src/agui/map.rs) for any `AgentEvent` with no
    // direct AG-UI equivalent, including unknown (`#[non_exhaustive]`) variants.
    pub(crate) fn custom(name: &str, value: Value) -> Value {
        json!({"type": "CUSTOM", "name": name, "value": value})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{ContentPart, Item};

    #[test]
    fn deserializes_the_documented_run_agent_input_shape() {
        let raw = r#"{
            "threadId": "thread-123",
            "runId": "run-456",
            "messages": [{"id": "msg-1", "role": "user", "content": "Hello, agent!"}],
            "tools": [],
            "context": [],
            "state": {},
            "forwardedProps": {}
        }"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        assert_eq!(input.thread_id.as_deref(), Some("thread-123"));
        assert_eq!(input.run_id.as_deref(), Some("run-456"));
        assert_eq!(input.messages.len(), 1);
    }

    #[test]
    fn unknown_fields_are_ignored_not_rejected() {
        let raw = r#"{"messages": [], "somethingBrandNew": {"a": 1}}"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        assert!(input.messages.is_empty());
    }

    #[test]
    fn maps_roles_onto_items() {
        let raw = r#"{"messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"hello"},
            {"role":"system","content":"be nice"}
        ]}"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        let agent_input = input.into_agent_input();
        assert_eq!(agent_input.messages.len(), 3);
        assert!(matches!(agent_input.messages[0], Item::UserMessage { .. }));
        assert!(matches!(
            agent_input.messages[1],
            Item::AssistantMessage { .. }
        ));
        assert!(matches!(agent_input.messages[2], Item::System { .. }));
        let Item::UserMessage { content } = &agent_input.messages[0] else {
            panic!("expected a user message");
        };
        assert!(matches!(&content[0], ContentPart::Text { text } if text == "hi"));
    }

    #[test]
    fn messages_without_content_are_skipped() {
        let raw = r#"{"messages":[{"role":"user"},{"role":"user","content":"real"}]}"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        assert_eq!(input.into_agent_input().messages.len(), 1);
    }

    #[test]
    fn event_constructors_use_the_documented_field_names() {
        let e = event::run_started("t1", "r1");
        assert_eq!(e["type"], "RUN_STARTED");
        assert_eq!(e["threadId"], "t1");
        assert_eq!(e["runId"], "r1");

        let e = event::text_message_content("m0", "chunk");
        assert_eq!(e["type"], "TEXT_MESSAGE_CONTENT");
        assert_eq!(e["messageId"], "m0");
        assert_eq!(e["delta"], "chunk");

        let e = event::tool_call_start("tc1", "search", "m0");
        assert_eq!(e["type"], "TOOL_CALL_START");
        assert_eq!(e["toolCallId"], "tc1");
        assert_eq!(e["toolCallName"], "search");
        assert_eq!(e["parentMessageId"], "m0");

        let e = event::custom("helikon.guardrail", serde_json::json!({"kind": "input"}));
        assert_eq!(e["type"], "CUSTOM");
        assert_eq!(e["name"], "helikon.guardrail");
        assert_eq!(e["value"]["kind"], "input");
    }
}

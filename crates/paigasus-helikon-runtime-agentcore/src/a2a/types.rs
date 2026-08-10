//! A2A wire types: the JSON-RPC 2.0 envelope and the task/artifact/agent-card shapes.
//!
//! Field names follow the A2A specification's camelCase wire format, which is why every
//! struct here carries `#[serde(rename_all = "camelCase")]` rather than relying on Rust's
//! snake_case field names.
//!
//! Requests are deliberately permissive: no `deny_unknown_fields` anywhere, and
//! [`Part`] has a catch-all variant, so a client sending specification fields this
//! runtime does not model (or a part kind added after this was written) is answered
//! rather than rejected at the parse step.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC error codes emitted by this container.
///
/// These are **A2A-specification** codes. AWS additionally publishes a `-32051`…`-32055`
/// table in its AgentCore A2A documentation; those describe what the *platform* returns
/// to a client (throttling, runtime unavailable, and so on) and are produced by AWS in
/// front of this container. Emitting one from here would claim a platform condition that
/// did not happen, so they must never appear in this module — a regression test in this
/// file asserts exactly that.
pub(crate) mod rpc_error {
    /// Invalid JSON was received (JSON-RPC 2.0 core).
    pub(crate) const PARSE_ERROR: i32 = -32700;
    /// The payload was valid JSON but not a valid request object (JSON-RPC 2.0 core).
    pub(crate) const INVALID_REQUEST: i32 = -32600;
    /// The method does not exist (JSON-RPC 2.0 core).
    pub(crate) const METHOD_NOT_FOUND: i32 = -32601;
    /// The method's parameters were invalid (JSON-RPC 2.0 core).
    pub(crate) const INVALID_PARAMS: i32 = -32602;
    /// An internal error occurred while handling the request (JSON-RPC 2.0 core).
    pub(crate) const INTERNAL_ERROR: i32 = -32603;
    /// The referenced task id is unknown (A2A `TaskNotFoundError`).
    pub(crate) const TASK_NOT_FOUND: i32 = -32001;
    /// The task exists but is not in a cancelable state (A2A `TaskNotCancelableError`).
    pub(crate) const TASK_NOT_CANCELABLE: i32 = -32002;
    /// Push notifications are not supported by this agent
    /// (A2A `PushNotificationNotSupportedError`).
    pub(crate) const PUSH_NOTIFICATION_NOT_SUPPORTED: i32 = -32003;
    /// The requested operation is not supported by this agent
    /// (A2A `UnsupportedOperationError`).
    pub(crate) const UNSUPPORTED_OPERATION: i32 = -32004;
    /// The message carried a content type this agent cannot process
    /// (A2A `ContentTypeNotSupportedError`).
    pub(crate) const CONTENT_TYPE_NOT_SUPPORTED: i32 = -32005;
}

/// Current wall-clock time as an RFC 3339 timestamp, for [`TaskStatus::timestamp`].
pub(crate) fn now_rfc3339() -> String {
    jiff::Timestamp::now().to_string()
}

// ── Task ──────────────────────────────────────────────────────────────────────

/// Lifecycle state of an A2A task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// Accepted but not yet started.
    Submitted,
    /// Currently executing.
    Working,
    /// Paused awaiting further client input. Never produced by this runtime, which has
    /// no interrupt seam; accepted so a custom [`TaskStore`](crate::TaskStore) that does
    /// model it round-trips.
    #[serde(rename = "input-required")]
    InputRequired,
    /// Finished successfully.
    Completed,
    /// Cancelled before it finished.
    Canceled,
    /// Finished with an error.
    Failed,
}

impl TaskState {
    /// Every non-terminal state — equivalently, every state a task can legally be
    /// cancelled *from*.
    ///
    /// `tasks/cancel` walks this list rather than naming states inline, because it
    /// cannot know which one a task is in: the run's own driver advances the task
    /// concurrently, so a single compare-and-swap against a guessed state loses the
    /// race. Enumerating them in one place is also what stops a state being forgotten —
    /// an earlier fix handled `Submitted` and silently missed `InputRequired`, leaving
    /// such tasks uncancellable behind a "not cancelable" error.
    pub const NON_TERMINAL: &'static [TaskState] =
        &[Self::Submitted, Self::Working, Self::InputRequired];

    /// Whether this state is final — no further transition is legal from here, and a
    /// `subscribe` stream ends once the task reaches one.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Canceled | Self::Failed)
    }
}

/// A task's state plus the moment it was last changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    /// The task's lifecycle state.
    pub state: TaskState,
    /// RFC 3339 timestamp of the most recent state change.
    pub timestamp: String,
}

/// Discriminator field the A2A specification requires on a task object.
///
/// A single-variant enum rather than a hardcoded string so the `"kind": "task"` literal
/// lives in exactly one place and serde emits it automatically.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    /// The only legal value: `"task"`.
    Task,
}

/// One part of a message or artifact.
///
/// `#[serde(tag = "kind")]` matches the specification's discriminated-union wire form.
/// The [`Other`](Part::Other) catch-all means an unmodelled part kind deserializes
/// instead of failing the whole request, letting the handler answer with a precise
/// "content type not supported" error rather than a parse error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Part {
    /// A plain-text part — the only kind this runtime processes.
    Text {
        /// The text content.
        text: String,
    },
    /// Any other part kind (file, data, or something added to the specification later).
    #[serde(other)]
    Other,
}

/// A unit of output produced by a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Stable identifier for this artifact within its task.
    pub artifact_id: String,
    /// Human-readable name.
    pub name: String,
    /// The artifact's content.
    pub parts: Vec<Part>,
}

/// An A2A task: one unit of work with a lifecycle, addressable by id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Server-assigned task identifier.
    pub id: String,
    /// Conversation identifier grouping related tasks.
    pub context_id: String,
    /// Current lifecycle state.
    pub status: TaskStatus,
    /// Output produced so far. Empty until the run yields text.
    pub artifacts: Vec<Artifact>,
    /// Always [`TaskKind::Task`]; part of the wire contract.
    pub kind: TaskKind,
}

/// One entry in a task's event log, as replayed by
/// [`TaskStore::subscribe`](crate::TaskStore::subscribe).
///
/// The payload is an already-shaped A2A streaming event (a `status-update` or
/// `artifact-update` object) rather than a typed enum: it goes straight out as an SSE
/// `data:` frame, and a typed representation would be converted right back.
#[derive(Debug, Clone)]
pub struct TaskEvent {
    /// Position in the task's event log, assigned by the store on append — never by the
    /// caller, whose value is ignored.
    pub seq: u64,
    /// The A2A streaming event to emit.
    pub payload: Value,
}

// ── Messages ──────────────────────────────────────────────────────────────────

/// An inbound A2A message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct A2aMessage {
    /// Sender role, e.g. `"user"`.
    // Never read: an inbound `message/send` is a turn *from* the client by definition,
    // so this runtime treats every request message as user input regardless of the
    // declared role. Kept because it is a required field of A2A's message shape and a
    // test asserts it round-trips.
    #[allow(dead_code)]
    pub(crate) role: String,
    /// The message content.
    #[serde(default)]
    pub(crate) parts: Vec<Part>,
    /// Client-assigned message id. Unused by this runtime.
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) message_id: Option<String>,
    /// Continues an existing task when present.
    #[serde(default)]
    pub(crate) task_id: Option<String>,
    /// Client-proposed conversation id. The platform session header wins over this.
    #[serde(default)]
    pub(crate) context_id: Option<String>,
}

impl A2aMessage {
    /// Concatenate every text part, ignoring all others.
    pub(crate) fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                Part::Other => None,
            })
            .collect()
    }

    /// Whether any part is something other than text — the trigger for answering
    /// `CONTENT_TYPE_NOT_SUPPORTED` rather than silently dropping content.
    pub(crate) fn has_non_text_parts(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::Other))
    }
}

/// Parameters of `message/send` and `message/stream`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageSendParams {
    /// The message to process.
    pub(crate) message: A2aMessage,
}

/// Parameters of `tasks/get`, `tasks/cancel`, and `tasks/resubscribe`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskIdParams {
    /// The task to address.
    pub(crate) id: String,
}

// ── JSON-RPC envelope ─────────────────────────────────────────────────────────

/// An inbound JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
pub(crate) struct JsonRpcRequest {
    /// Protocol version. Anything but `"2.0"` is an invalid request.
    pub(crate) jsonrpc: String,
    /// Correlation id, echoed on the response. Absent for notifications.
    #[serde(default)]
    pub(crate) id: Option<Value>,
    /// The method being invoked, e.g. `"message/send"`.
    pub(crate) method: String,
    /// Method-specific parameters, decoded per method.
    #[serde(default)]
    pub(crate) params: Option<Value>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcError {
    /// Numeric error code — see [`rpc_error`].
    pub(crate) code: i32,
    /// Human-readable description.
    pub(crate) message: String,
}

/// A JSON-RPC 2.0 response. Exactly one of `result`/`error` is populated.
#[derive(Debug, Serialize)]
pub(crate) struct JsonRpcResponse {
    /// Always `"2.0"`.
    pub(crate) jsonrpc: &'static str,
    /// The request's correlation id, echoed back.
    pub(crate) id: Value,
    /// The successful result, when the call succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
    /// The failure, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// A successful response carrying `result`.
    pub(crate) fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response carrying `code`/`message` and no result.
    pub(crate) fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

// ── Agent card ────────────────────────────────────────────────────────────────

/// Which optional A2A transports this agent supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Whether `message/stream` and `tasks/resubscribe` are available. Always `true`
    /// for a card this crate derives.
    pub streaming: bool,
}

/// One advertised capability of an agent, for discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    /// Stable identifier for the skill.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// What the skill does.
    pub description: String,
    /// Free-form tags aiding discovery.
    pub tags: Vec<String>,
}

/// The A2A agent card served at `/.well-known/agent-card.json`.
///
/// Construct one by hand and install it with
/// [`AgentCoreServerBuilder::agent_card`](crate::AgentCoreServerBuilder::agent_card) to
/// override the card this crate derives from the configured agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    /// The agent's name.
    pub name: String,
    /// What the agent does.
    pub description: String,
    /// The agent's version.
    pub version: String,
    /// Where the agent is reachable.
    ///
    /// `None` omits the field entirely. A bind address such as `0.0.0.0` is not a
    /// routable URL, so publishing one would actively mislead a discovering client —
    /// an absent url is the honest answer when nothing authoritative is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A2A protocol version this card describes.
    pub protocol_version: String,
    /// Transport a client should prefer, e.g. `"JSONRPC"`.
    pub preferred_transport: String,
    /// Optional transports this agent supports.
    pub capabilities: AgentCapabilities,
    /// Input content types accepted, e.g. `["text"]`.
    pub default_input_modes: Vec<String>,
    /// Output content types produced, e.g. `["text"]`.
    pub default_output_modes: Vec<String>,
    /// Advertised skills.
    pub skills: Vec<AgentSkill>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_serializes_in_the_documented_shape() {
        let task = Task {
            id: "task-1".to_owned(),
            context_id: "ctx-1".to_owned(),
            status: TaskStatus {
                state: TaskState::Completed,
                timestamp: "2026-08-08T09:00:00Z".to_owned(),
            },
            artifacts: vec![Artifact {
                artifact_id: "art-1".to_owned(),
                name: "agent_response".to_owned(),
                parts: vec![Part::Text {
                    text: "hello".to_owned(),
                }],
            }],
            kind: TaskKind::Task,
        };
        let v = serde_json::to_value(&task).unwrap();
        assert_eq!(v["id"], "task-1");
        assert_eq!(v["contextId"], "ctx-1");
        assert_eq!(v["status"]["state"], "completed");
        assert_eq!(v["kind"], "task");
        assert_eq!(v["artifacts"][0]["artifactId"], "art-1");
        assert_eq!(v["artifacts"][0]["parts"][0]["kind"], "text");
        assert_eq!(v["artifacts"][0]["parts"][0]["text"], "hello");
    }

    /// `NON_TERMINAL` and `is_terminal` must stay exact complements. The `match` below
    /// is exhaustive on purpose: adding a variant to [`TaskState`] fails to compile
    /// here, which is the reminder to classify it in both places.
    #[test]
    fn non_terminal_is_exactly_the_complement_of_is_terminal() {
        fn terminal_by_exhaustive_match(s: TaskState) -> bool {
            match s {
                TaskState::Submitted | TaskState::Working | TaskState::InputRequired => false,
                TaskState::Completed | TaskState::Canceled | TaskState::Failed => true,
            }
        }

        for state in [
            TaskState::Submitted,
            TaskState::Working,
            TaskState::InputRequired,
            TaskState::Completed,
            TaskState::Canceled,
            TaskState::Failed,
        ] {
            assert_eq!(
                state.is_terminal(),
                terminal_by_exhaustive_match(state),
                "{state:?}: is_terminal disagrees with the exhaustive classification"
            );
            assert_eq!(
                TaskState::NON_TERMINAL.contains(&state),
                !state.is_terminal(),
                "{state:?}: NON_TERMINAL membership disagrees with is_terminal"
            );
        }
    }

    #[test]
    fn task_state_terminality_is_correct() {
        assert!(!TaskState::Submitted.is_terminal());
        assert!(!TaskState::Working.is_terminal());
        assert!(!TaskState::InputRequired.is_terminal());
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(TaskState::Failed.is_terminal());
    }

    #[test]
    fn parses_the_documented_message_send_request() {
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": "req-001",
            "method": "message/send",
            "params": {"message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "Your message content here"}],
                "messageId": "unique-message-id"
            }}
        }"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.method, "message/send");
        let params: MessageSendParams = serde_json::from_value(req.params.unwrap()).unwrap();
        assert_eq!(params.message.role, "user");
        assert_eq!(params.message.text(), "Your message content here");
        assert!(params.message.task_id.is_none());
    }

    #[test]
    fn message_text_concatenates_text_parts_only() {
        let raw = r#"{"role":"user","parts":[
            {"kind":"text","text":"a"},
            {"kind":"text","text":"b"}
        ],"messageId":"m"}"#;
        let m: A2aMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(m.text(), "ab");
        assert!(!m.has_non_text_parts());
    }

    #[test]
    fn non_text_parts_are_detected() {
        let raw = r#"{"role":"user","parts":[
            {"kind":"file","file":{"uri":"s3://x"}}
        ],"messageId":"m"}"#;
        let m: A2aMessage = serde_json::from_str(raw).unwrap();
        assert!(m.has_non_text_parts());
    }

    #[test]
    fn error_responses_use_a2a_specification_codes() {
        let resp = JsonRpcResponse::error(
            serde_json::json!("req-001"),
            rpc_error::TASK_NOT_FOUND,
            "Task not found",
        );
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], "req-001");
        assert_eq!(v["error"]["code"], -32001);
        assert!(
            v.get("result").is_none(),
            "an error response carries no result"
        );
    }

    /// Guard against re-introducing AWS's platform-side table (§5.6): those codes
    /// describe what the *platform* returns to a client, never what this container emits.
    #[test]
    fn specification_codes_are_not_the_aws_platform_codes() {
        assert_eq!(rpc_error::TASK_NOT_FOUND, -32001);
        assert_eq!(rpc_error::TASK_NOT_CANCELABLE, -32002);
        assert_eq!(rpc_error::PUSH_NOTIFICATION_NOT_SUPPORTED, -32003);
        assert_eq!(rpc_error::UNSUPPORTED_OPERATION, -32004);
        assert_eq!(rpc_error::CONTENT_TYPE_NOT_SUPPORTED, -32005);
        assert_eq!(rpc_error::METHOD_NOT_FOUND, -32601);
        for code in [-32051, -32052, -32053, -32054, -32055] {
            assert!(
                ![
                    rpc_error::TASK_NOT_FOUND,
                    rpc_error::TASK_NOT_CANCELABLE,
                    rpc_error::PUSH_NOTIFICATION_NOT_SUPPORTED,
                    rpc_error::UNSUPPORTED_OPERATION,
                    rpc_error::CONTENT_TYPE_NOT_SUPPORTED,
                    rpc_error::METHOD_NOT_FOUND,
                    rpc_error::INVALID_PARAMS,
                    rpc_error::INTERNAL_ERROR,
                    rpc_error::PARSE_ERROR,
                    rpc_error::INVALID_REQUEST,
                ]
                .contains(&code),
                "{code} is an AWS platform code and must not appear in this container"
            );
        }
    }

    #[test]
    fn agent_card_serializes_with_the_documented_field_names() {
        let card = AgentCard {
            name: "n".to_owned(),
            description: "d".to_owned(),
            version: "1.0.0".to_owned(),
            url: None,
            protocol_version: "0.3.0".to_owned(),
            preferred_transport: "JSONRPC".to_owned(),
            capabilities: AgentCapabilities { streaming: true },
            default_input_modes: vec!["text".to_owned()],
            default_output_modes: vec!["text".to_owned()],
            skills: vec![AgentSkill {
                id: "n".to_owned(),
                name: "n".to_owned(),
                description: "d".to_owned(),
                tags: vec![],
            }],
        };
        let v = serde_json::to_value(&card).unwrap();
        assert_eq!(v["protocolVersion"], "0.3.0");
        assert_eq!(v["preferredTransport"], "JSONRPC");
        assert_eq!(v["capabilities"]["streaming"], true);
        assert_eq!(v["defaultInputModes"][0], "text");
        assert!(
            v.get("url").is_none(),
            "an unknown url must be omitted, never published as 0.0.0.0"
        );
    }
}

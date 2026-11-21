//! Translation between Paigasus carrier types and Anthropic wire format.

pub(crate) mod cache;
pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tools;

use paigasus_helikon_core::{ModelError, ModelRequest, ToolChoice};
use serde_json::{json, Value};

use crate::builder::Config;
use crate::settings::ExtendedThinking;
use response_format::{
    synthesize_for_response_format, validate_conflict, validate_tool_names, Synthesized,
};

/// Built request body + whether the stream translator should be in synthesis mode.
#[derive(Debug)]
pub(crate) struct PreparedRequest {
    pub(crate) body: Value,
    pub(crate) synthesizing_output: bool,
}

/// Build the JSON request body from the caller's `ModelRequest` plus the
/// builder-baked `Config`. Runs all synchronous validation guards.
pub(crate) fn build_body(cfg: &Config, req: &ModelRequest) -> Result<PreparedRequest, ModelError> {
    validate_tool_names(&req.tools).map_err(|m| ModelError::Other(anyhow::anyhow!(m)))?;
    validate_conflict(
        req.model_settings.response_format.as_ref(),
        req.model_settings.tool_choice.as_ref(),
    )
    .map_err(|m| ModelError::Other(anyhow::anyhow!(m)))?;

    let translated = request::translate_messages(&req.messages);
    if translated.messages.as_array().is_none_or(|a| a.is_empty()) {
        return Err(ModelError::Other(anyhow::anyhow!(
            "no message with content to send (empty or whitespace-only text and unsupported parts are not sent)"
        )));
    }
    if translated.trailing_message_dropped {
        return Err(ModelError::Other(anyhow::anyhow!(
            "the last message has no content to send (empty or whitespace-only text and unsupported parts are not sent); the request would end with an assistant message"
        )));
    }

    let mut tools_array = tools::translate_tools(&req.tools, cfg.native_tool_types);

    let Synthesized {
        tool,
        tool_choice,
        synthesizing,
    } = synthesize_for_response_format(req.model_settings.response_format.as_ref());
    if let Some(extra) = tool {
        if let Some(arr) = tools_array.as_array_mut() {
            arr.push(extra);
        }
    }

    let mut messages = translated.messages;
    let system = cache::apply_cache_strategy(
        cfg.cache_strategy,
        translated.system,
        &mut tools_array,
        &mut messages,
    );

    let mut body = serde_json::Map::new();
    body.insert("model".into(), Value::String(cfg.model_id.clone()));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("messages".into(), messages);
    body.insert(
        "max_tokens".into(),
        Value::Number(
            req.model_settings
                .max_output_tokens
                .map(|m| m.into())
                .unwrap_or_else(|| cfg.max_output_default.into()),
        ),
    );
    if let Some(s) = system {
        body.insert("system".into(), s);
    }
    if tools_array
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false)
    {
        body.insert("tools".into(), tools_array);
    }

    // tool_choice: synthesis overrides caller; otherwise translate caller's preference.
    let tc_value = match tool_choice {
        Some(v) => Some(v),
        None => req
            .model_settings
            .tool_choice
            .as_ref()
            .map(translate_tool_choice),
    };
    if let Some(v) = tc_value {
        body.insert("tool_choice".into(), v);
    }

    if let Some(t) = req.model_settings.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.model_settings.top_p {
        body.insert("top_p".into(), json!(p));
    }
    if let Some(k) = cfg.top_k {
        body.insert("top_k".into(), json!(k));
    }
    match cfg.extended_thinking {
        ExtendedThinking::Disabled => {}
        ExtendedThinking::Enabled { budget_tokens } => {
            body.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget_tokens}),
            );
        }
        ExtendedThinking::Adaptive => {
            body.insert("thinking".into(), json!({"type": "adaptive"}));
        }
    }

    if req.model_settings.previous_response_id.is_some() {
        tracing::debug!(
            target: "paigasus::anthropic::translate",
            "previous_response_id is Anthropic-irrelevant; ignoring",
        );
    }

    Ok(PreparedRequest {
        body: Value::Object(body),
        synthesizing_output: synthesizing,
    })
}

fn translate_tool_choice(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => json!({"type": "auto"}),
        ToolChoice::Required => json!({"type": "any"}),
        ToolChoice::None => json!({"type": "none"}),
        ToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
        _ => json!({"type": "auto"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::AnthropicModelBuilder;
    use paigasus_helikon_core::{ContentPart, Item, ModelRequest, ResponseFormat, ToolDef};

    fn cfg() -> Config {
        AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-test")
            .build_config()
            .unwrap()
    }

    fn user_text(s: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: s.to_owned() }],
        }
    }

    fn asst_text(s: &str) -> Item {
        Item::AssistantMessage {
            content: vec![ContentPart::Text { text: s.to_owned() }],
            agent: None,
        }
    }

    fn sys_text(s: &str) -> Item {
        Item::System {
            content: vec![ContentPart::Text { text: s.to_owned() }],
        }
    }

    fn tool_call(id: &str) -> Item {
        Item::ToolCall {
            call_id: id.to_owned(),
            name: "ping".to_owned(),
            args: json!({}),
        }
    }

    fn tool_result(id: &str) -> Item {
        Item::ToolResult {
            call_id: id.to_owned(),
            content: vec![ContentPart::Text {
                text: "pong".to_owned(),
            }],
        }
    }

    fn build_err(items: Vec<Item>) -> ModelError {
        let mut req = ModelRequest::new();
        req.messages = items;
        build_body(&cfg(), &req).unwrap_err()
    }

    #[test]
    fn blank_only_user_is_rejected() {
        let err = build_err(vec![user_text("")]);
        assert!(matches!(err, ModelError::Other(_)));
        assert!(err.to_string().contains("no message with content"));
    }

    #[test]
    fn system_only_is_rejected() {
        let err = build_err(vec![sys_text("be nice")]);
        assert!(matches!(err, ModelError::Other(_)));
        assert!(err.to_string().contains("no message with content"));
    }

    #[test]
    fn system_and_blank_user_is_rejected() {
        let err = build_err(vec![sys_text("be nice"), user_text(" ")]);
        assert!(matches!(err, ModelError::Other(_)));
        assert!(err.to_string().contains("no message with content"));
    }

    #[test]
    fn trailing_blank_user_after_assistant_is_rejected() {
        let err = build_err(vec![user_text("q"), asst_text("a"), user_text("")]);
        assert!(matches!(err, ModelError::Other(_)));
        assert!(err.to_string().contains("would end with an assistant"));
    }

    #[test]
    fn caller_prefill_after_blank_user_is_sent() {
        let mut req = ModelRequest::new();
        req.messages = vec![
            user_text("q"),
            asst_text("a"),
            user_text(""),
            asst_text("b"),
        ];
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(
            p.body["messages"],
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "a"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "b"}]},
            ])
        );
    }

    #[test]
    fn trailing_blank_assistant_after_blank_user_is_rejected() {
        let err = build_err(vec![
            user_text("q"),
            asst_text("a"),
            user_text(""),
            asst_text(""),
        ]);
        assert!(matches!(err, ModelError::Other(_)));
        assert!(err.to_string().contains("would end with an assistant"));
    }

    #[test]
    fn blank_user_after_tool_result_is_sent() {
        let mut req = ModelRequest::new();
        req.messages = vec![
            user_text("q"),
            asst_text("a"),
            tool_call("tu_1"),
            tool_result("tu_1"),
            user_text(""),
        ];
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(
            p.body["messages"],
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "a"},
                    {"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"},
                ]},
            ])
        );
    }

    #[test]
    fn last_turn_cache_skips_dropped_blank_user() {
        let cfg = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-test")
            .cache_strategy(crate::settings::CacheStrategy::LastTurn)
            .build_config()
            .unwrap();
        let mut req = ModelRequest::new();
        req.messages = vec![
            user_text("q"),
            asst_text("a"),
            tool_call("tu_1"),
            tool_result("tu_1"),
            user_text(""),
        ];
        let p = build_body(&cfg, &req).unwrap();
        let msgs = p.body["messages"].as_array().unwrap();
        let last_block = msgs.last().unwrap()["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last_block["type"], "tool_result");
        assert_eq!(last_block["tool_use_id"], "tu_1");
        assert!(last_block.get("cache_control").is_some());
        for m in msgs {
            for b in m["content"].as_array().unwrap() {
                if b["type"] == "text" {
                    assert!(!b["text"].as_str().unwrap().trim().is_empty());
                }
            }
        }
    }

    #[test]
    fn basic_request_has_model_messages_max_tokens_stream() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(p.body["model"], "claude-sonnet-4-6");
        assert_eq!(p.body["stream"], true);
        assert!(p.body["messages"].is_array());
        assert_eq!(p.body["max_tokens"], 32_768);
        assert!(!p.synthesizing_output);
    }

    #[test]
    fn caller_max_tokens_overrides_model_default() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.model_settings.max_output_tokens = Some(1024);
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(p.body["max_tokens"], 1024);
    }

    #[test]
    fn tool_choice_none_emits_native_none() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("search", "", serde_json::json!({}))];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(p.body["tool_choice"], serde_json::json!({"type": "none"}));
        assert!(
            p.body["tools"].is_array(),
            "tools stay in body so prefix matches cached turns"
        );
    }

    #[test]
    fn json_schema_synthesizes_forced_tool() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Build a person.")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: serde_json::json!({"type": "object"}),
            strict: false,
        });
        let p = build_body(&cfg(), &req).unwrap();
        assert!(p.synthesizing_output);
        assert_eq!(
            p.body["tool_choice"],
            serde_json::json!({"type": "tool", "name": "__paigasus_structured_output__"}),
        );
        let tools = p.body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "__paigasus_structured_output__");
    }

    #[test]
    fn reserved_tool_name_rejected_synchronously() {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new(
            "__paigasus_structured_output__",
            "",
            serde_json::json!({}),
        )];
        let err = build_body(&cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn extended_thinking_adaptive_emits_adaptive_payload() {
        let mut cfg = cfg();
        cfg.extended_thinking = ExtendedThinking::Adaptive;
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        let p = build_body(&cfg, &req).unwrap();
        assert_eq!(p.body["thinking"], serde_json::json!({"type": "adaptive"}));
    }

    fn memory_def() -> ToolDef {
        ToolDef::new(
            "memory",
            "custom description",
            serde_json::json!({"type":"object","properties":{"command":{"type":"string"}}}),
        )
        .with_provider_types(vec![paigasus_helikon_core::ProviderToolType::new(
            crate::PROVIDER_ID,
            "memory_20250818",
        )])
    }

    #[test]
    fn cache_marker_lands_on_typed_entry_when_last() {
        let mut cfg = cfg();
        cfg.cache_strategy = crate::settings::CacheStrategy::Tools;
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![memory_def()];
        let p = build_body(&cfg, &req).unwrap();
        assert_eq!(
            p.body["tools"][0],
            serde_json::json!({"type":"memory_20250818","name":"memory","cache_control":{"type":"ephemeral"}})
        );
    }

    #[test]
    fn structured_output_tool_takes_marker_and_typed_entry_unchanged() {
        let mut cfg = cfg();
        cfg.cache_strategy = crate::settings::CacheStrategy::Tools;
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![memory_def()];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: serde_json::json!({"type": "object"}),
            strict: false,
        });
        let p = build_body(&cfg, &req).unwrap();
        let tools = p.body["tools"].as_array().unwrap();
        let last = tools.last().unwrap();
        assert_eq!(last["name"], "__paigasus_structured_output__");
        assert_eq!(
            last["cache_control"],
            serde_json::json!({"type":"ephemeral"})
        );
        assert_eq!(
            tools[0],
            serde_json::json!({"type":"memory_20250818","name":"memory"})
        );
    }

    #[test]
    fn builder_opt_out_reaches_body() {
        let mut cfg = cfg();
        cfg.native_tool_types = false;
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![memory_def()];
        let p = build_body(&cfg, &req).unwrap();
        assert!(p.body["tools"][0].get("input_schema").is_some());
        assert!(p.body["tools"][0].get("type").is_none());
    }

    #[test]
    fn public_setter_opt_out_reaches_body() {
        let cfg = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-test")
            .native_tool_types(false)
            .build_config()
            .unwrap();
        assert!(!cfg.native_tool_types);
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![memory_def()];
        let p = build_body(&cfg, &req).unwrap();
        assert!(p.body["tools"][0].get("input_schema").is_some());
        assert!(p.body["tools"][0].get("type").is_none());
    }

    #[test]
    fn native_tool_types_defaults_to_true() {
        assert!(cfg().native_tool_types);
    }
}

/// Request bodies recorded from the code before SMA-840.
/// Each history has no Anthropic provider data, so its body must not change.
#[cfg(test)]
mod baseline {
    use super::*;
    use crate::builder::AnthropicModelBuilder;
    use crate::settings::CacheStrategy;
    use paigasus_helikon_core::{ContentPart, Item, ModelRequest};

    fn builder() -> AnthropicModelBuilder {
        AnthropicModelBuilder::new("claude-sonnet-4-6").api_key("sk-test")
    }

    fn default_cfg() -> Config {
        builder().build_config().unwrap()
    }

    /// Build the request body for `items` and return it as JSON.
    fn baseline_body(cfg: &Config, items: Vec<Item>) -> serde_json::Value {
        let mut req = ModelRequest::new();
        req.messages = items;
        build_body(cfg, &req).unwrap().body
    }

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    fn reasoning(t: &str) -> ContentPart {
        ContentPart::Reasoning { text: t.to_owned() }
    }

    fn user(t: &str) -> Item {
        Item::UserMessage {
            content: vec![text(t)],
        }
    }

    fn asst(t: &str) -> Item {
        asst_parts(vec![text(t)])
    }

    fn asst_parts(content: Vec<ContentPart>) -> Item {
        Item::AssistantMessage {
            content,
            agent: None,
        }
    }

    fn call(id: &str) -> Item {
        Item::ToolCall {
            call_id: id.into(),
            name: "ping".into(),
            args: json!({}),
        }
    }

    fn result(id: &str) -> Item {
        Item::ToolResult {
            call_id: id.into(),
            content: vec![text("pong")],
        }
    }

    fn use_part(id: &str) -> ContentPart {
        ContentPart::ToolUse {
            call_id: id.into(),
            name: "ping".into(),
            args: json!({}),
        }
    }

    fn tool_loop_items() -> Vec<Item> {
        vec![
            user("q"),
            asst_parts(vec![reasoning("r"), text("t")]),
            call("tu_1"),
            result("tu_1"),
            asst("done"),
        ]
    }

    #[test]
    fn baseline_text_turns() {
        let items = vec![user("q"), asst("a"), user("r")];
        insta::assert_json_snapshot!("baseline_text_turns", baseline_body(&default_cfg(), items));
    }

    #[test]
    fn baseline_accumulator_tool_loop() {
        insta::assert_json_snapshot!(
            "baseline_accumulator_tool_loop",
            baseline_body(&default_cfg(), tool_loop_items())
        );
    }

    #[test]
    fn baseline_nested_open_round() {
        let items = vec![
            user("q"),
            asst_parts(vec![use_part("t1"), use_part("t2")]),
            result("t1"),
            asst_parts(vec![text("b"), use_part("t3")]),
            result("t2"),
            result("t3"),
        ];
        insta::assert_json_snapshot!(
            "baseline_nested_open_round",
            baseline_body(&default_cfg(), items)
        );
    }

    #[test]
    fn baseline_handoff_transcript() {
        let items = vec![
            user("q"),
            asst_parts(vec![reasoning("r")]),
            user("You are now handling a transferred conversation. Continue assisting the user."),
        ];
        insta::assert_json_snapshot!(
            "baseline_handoff_transcript",
            baseline_body(&default_cfg(), items)
        );
    }

    #[test]
    fn baseline_transfer_split() {
        let items = vec![
            user("q"),
            asst_parts(vec![text("t")]),
            call("a"),
            result("a"),
            call("b"),
            result("b"),
        ];
        insta::assert_json_snapshot!(
            "baseline_transfer_split",
            baseline_body(&default_cfg(), items)
        );
    }

    #[test]
    fn baseline_foreign_provider_data() {
        let pd = || ContentPart::ProviderData {
            provider: "gemini".to_owned(),
            call_id: None,
            data: json!({"sig": "SIG_A"}),
        };
        let items = vec![
            Item::UserMessage {
                content: vec![text("q"), pd()],
            },
            asst_parts(vec![pd()]),
            asst_parts(vec![text("a"), pd()]),
        ];
        insta::assert_json_snapshot!(
            "baseline_foreign_provider_data",
            baseline_body(&default_cfg(), items)
        );
    }

    #[test]
    fn baseline_last_turn_cache() {
        let cfg = builder()
            .cache_strategy(CacheStrategy::LastTurn)
            .build_config()
            .unwrap();
        insta::assert_json_snapshot!(
            "baseline_last_turn_cache",
            baseline_body(&cfg, tool_loop_items())
        );
    }

    #[test]
    fn baseline_thinking_enabled() {
        let cfg = builder()
            .extended_thinking(ExtendedThinking::Enabled {
                budget_tokens: 2048,
            })
            .build_config()
            .unwrap();
        insta::assert_json_snapshot!(
            "baseline_thinking_enabled",
            baseline_body(&cfg, tool_loop_items())
        );
    }
}

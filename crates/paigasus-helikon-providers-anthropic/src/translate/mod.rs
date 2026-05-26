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

    let mut tools_array = tools::translate_tools(&req.tools);

    let Synthesized { tool, tool_choice, synthesizing } =
        synthesize_for_response_format(req.model_settings.response_format.as_ref());
    if let Some(extra) = tool {
        if let Some(arr) = tools_array.as_array_mut() {
            arr.push(extra);
        }
    }

    let mut messages = translated.messages;
    let system =
        cache::apply_cache_strategy(cfg.cache_strategy, translated.system, &mut tools_array, &mut messages);

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
    if tools_array.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
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

    Ok(PreparedRequest { body: Value::Object(body), synthesizing_output: synthesizing })
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
    use paigasus_helikon_core::{
        ContentPart, Item, ModelRequest, ResponseFormat, ToolDef,
    };

    fn cfg() -> Config {
        AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-test")
            .build_config()
            .unwrap()
    }

    fn user_text(s: &str) -> Item {
        Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
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
        req.tools = vec![ToolDef {
            name: "search".to_owned(),
            description: "".to_owned(),
            schema: serde_json::json!({}),
        }];
        req.model_settings.tool_choice = Some(ToolChoice::None);
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(p.body["tool_choice"], serde_json::json!({"type": "none"}));
        assert!(p.body["tools"].is_array(), "tools stay in body so prefix matches cached turns");
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
        req.tools = vec![ToolDef {
            name: "__paigasus_structured_output__".to_owned(),
            description: "".to_owned(),
            schema: serde_json::json!({}),
        }];
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
}

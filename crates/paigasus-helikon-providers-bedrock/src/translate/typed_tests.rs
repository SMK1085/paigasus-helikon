//! Tests for typed (native Anthropic) tools on the forced-tool path.

use insta::assert_json_snapshot;
use paigasus_helikon_core::{
    ContentPart, Item, ModelError, ModelRequest, ProviderToolType, ResponseFormat, ToolChoice,
    ToolDef,
};
use serde_json::{json, Value};

use crate::builder::Config;
use crate::policy::OutputPath;
use crate::translate::tools::{PLACEHOLDER_DESCRIPTION, SYNTHESIZED_TOOL_NAME};
use crate::translate::{build_request, to_wire_json, PreparedConverse};

const FORCED_ID: &str = "us.anthropic.claude-sonnet-4-6";
// remove in Task 5: no caller yet.
#[allow(dead_code)]
const NATIVE_ID: &str = "us.anthropic.claude-sonnet-5-5";
const NOVA_ID: &str = "us.amazon.nova-pro-v1:0";

pub(super) fn memory_tool() -> ToolDef {
    ToolDef::new("memory", "Memory", json!({"type": "object"}))
        .with_provider_types(vec![ProviderToolType::new("anthropic", "memory_20250818")])
}

pub(super) fn balance_tool() -> ToolDef {
    ToolDef::new(
        "get_balance",
        "Get account balance",
        json!({"type": "object", "properties": {"account_id": {"type": "string"}}}),
    )
}

fn text(t: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: t.to_owned() }]
}

/// A user turn, one `memory` call and its result.
pub(super) fn memory_round() -> Vec<Item> {
    vec![
        Item::UserMessage {
            content: text("Remember this."),
        },
        Item::AssistantMessage {
            content: vec![ContentPart::ToolUse {
                call_id: "tu_1".to_owned(),
                name: "memory".to_owned(),
                args: json!({"command": "view", "path": "/memories"}),
            }],
            agent: None,
        },
        Item::ToolResult {
            call_id: "tu_1".to_owned(),
            content: text("empty"),
        },
    ]
}

pub(super) fn cfg(id: &str) -> Config {
    crate::builder::offline_config(id)
}

pub(super) fn forced(cfg: &Config, req: &ModelRequest) -> Result<PreparedConverse, ModelError> {
    build_request(cfg, req, OutputPath::ForcedTool)
}

// remove in Task 5: no caller yet.
#[allow(dead_code)]
pub(super) fn native(cfg: &Config, req: &ModelRequest) -> Result<PreparedConverse, ModelError> {
    build_request(cfg, req, OutputPath::Native)
}

pub(super) fn tool_names(w: &Value) -> Vec<String> {
    w["toolConfig"]["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t["toolSpec"]["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn first_turn(tools: Vec<ToolDef>) -> ModelRequest {
    let mut req = ModelRequest::new();
    req.messages = vec![Item::UserMessage {
        content: text("Hello"),
    }];
    req.tools = tools;
    req
}

fn round(tools: Vec<ToolDef>) -> ModelRequest {
    let mut req = ModelRequest::new();
    req.messages = memory_round();
    req.tools = tools;
    req
}

fn typed_entry() -> Value {
    json!({"type": "memory_20250818", "name": "memory"})
}

fn err_text(r: Result<PreparedConverse, ModelError>) -> String {
    format!("{}", r.unwrap_err())
}

#[test]
fn snap_typed_only_first_turn() {
    let p = forced(&cfg(FORCED_ID), &first_turn(vec![memory_tool()])).unwrap();
    let w = to_wire_json(&p);
    assert!(w["toolConfig"].is_null());
    assert_eq!(
        w["additionalModelRequestFields"],
        json!({"tools": [typed_entry()]})
    );
    assert_json_snapshot!(w);
}

#[test]
fn snap_typed_and_generic() {
    let p = forced(
        &cfg(FORCED_ID),
        &first_turn(vec![memory_tool(), balance_tool()]),
    )
    .unwrap();
    let w = to_wire_json(&p);
    assert_eq!(tool_names(&w), vec!["get_balance".to_owned()]);
    assert!(w["toolConfig"]["toolChoice"].is_null());
    assert_eq!(
        w["additionalModelRequestFields"]["tools"],
        json!([typed_entry()])
    );
    assert_json_snapshot!(w);
}

#[test]
fn typed_auto_sends_no_choice() {
    let mut req = first_turn(vec![memory_tool(), balance_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Auto);
    let w = to_wire_json(&forced(&cfg(FORCED_ID), &req).unwrap());
    assert!(w["toolConfig"]["toolChoice"].is_null());
    assert!(w["additionalModelRequestFields"]
        .get("tool_choice")
        .is_none());
}

#[test]
fn snap_typed_required() {
    let mut req = first_turn(vec![memory_tool(), balance_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Required);
    let p = forced(&cfg(FORCED_ID), &req).unwrap();
    assert!(p.has_forced_tool_choice());
    let w = to_wire_json(&p);
    assert!(w["toolConfig"]["toolChoice"].is_null());
    assert_eq!(
        w["additionalModelRequestFields"]["tool_choice"],
        json!({"type": "any"})
    );
    assert_json_snapshot!(w);
}

#[test]
fn typed_required_with_only_typed_tools_is_valid() {
    let mut req = first_turn(vec![memory_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Required);
    let w = to_wire_json(&forced(&cfg(FORCED_ID), &req).unwrap());
    assert_eq!(
        w["additionalModelRequestFields"]["tool_choice"],
        json!({"type": "any"})
    );
}

#[test]
fn snap_typed_tool_choice_typed_name() {
    let mut req = first_turn(vec![memory_tool(), balance_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Tool {
        name: "memory".to_owned(),
    });
    let w = to_wire_json(&forced(&cfg(FORCED_ID), &req).unwrap());
    assert!(w["toolConfig"]["toolChoice"].is_null());
    assert_eq!(
        w["additionalModelRequestFields"]["tool_choice"],
        json!({"type": "tool", "name": "memory"})
    );
    assert_json_snapshot!(w);
}

#[test]
fn typed_tool_choice_generic_name() {
    let mut req = first_turn(vec![memory_tool(), balance_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Tool {
        name: "get_balance".to_owned(),
    });
    let w = to_wire_json(&forced(&cfg(FORCED_ID), &req).unwrap());
    assert!(w["toolConfig"]["toolChoice"].is_null());
    assert_eq!(
        w["additionalModelRequestFields"]["tool_choice"],
        json!({"type": "tool", "name": "get_balance"})
    );
}

#[test]
fn typed_tool_choice_unknown_name_is_error() {
    let mut req = first_turn(vec![memory_tool(), balance_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Tool {
        name: "nope".to_owned(),
    });
    assert!(err_text(forced(&cfg(FORCED_ID), &req)).contains("not defined in the tool list"));
}

#[test]
fn typed_tool_choice_cannot_select_the_placeholder() {
    let mut req = round(vec![memory_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Tool {
        name: SYNTHESIZED_TOOL_NAME.to_owned(),
    });
    assert!(err_text(forced(&cfg(FORCED_ID), &req)).contains("not defined in the tool list"));
}

#[test]
fn snap_typed_structured_output() {
    let mut req = first_turn(vec![memory_tool()]);
    req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Person".to_owned(),
        schema: json!({"type": "object"}),
        strict: false,
    });
    let p = forced(&cfg(FORCED_ID), &req).unwrap();
    let w = to_wire_json(&p);
    assert_eq!(tool_names(&w), vec![SYNTHESIZED_TOOL_NAME.to_owned()]);
    assert!(w["toolConfig"]["toolChoice"].is_null());
    assert_eq!(
        w["additionalModelRequestFields"]["tool_choice"],
        json!({"type": "tool", "name": SYNTHESIZED_TOOL_NAME})
    );
    assert_eq!(w["synthesizing"], json!(true));
    assert_json_snapshot!(w);
}

#[test]
fn snap_typed_only_tool_round_adds_placeholder() {
    let mut req = round(vec![memory_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::Auto);
    let w = to_wire_json(&forced(&cfg(FORCED_ID), &req).unwrap());
    assert_eq!(tool_names(&w), vec![SYNTHESIZED_TOOL_NAME.to_owned()]);
    assert_eq!(
        w["toolConfig"]["tools"][0]["toolSpec"]["description"],
        json!(PLACEHOLDER_DESCRIPTION)
    );
    assert_eq!(w["synthesizing"], json!(false));
    assert_json_snapshot!(w);
}

#[test]
fn typed_and_generic_tool_round_adds_no_placeholder() {
    let w = to_wire_json(
        &forced(&cfg(FORCED_ID), &round(vec![memory_tool(), balance_tool()])).unwrap(),
    );
    assert_eq!(tool_names(&w), vec!["get_balance".to_owned()]);
}

#[test]
fn snap_typed_none_after_tool_round() {
    let mut req = round(vec![memory_tool()]);
    req.model_settings.tool_choice = Some(ToolChoice::None);
    let w = to_wire_json(&forced(&cfg(FORCED_ID), &req).unwrap());
    assert_eq!(tool_names(&w), vec![SYNTHESIZED_TOOL_NAME.to_owned()]);
    assert_eq!(
        w["additionalModelRequestFields"]["tools"],
        json!([typed_entry()])
    );
    assert_eq!(
        w["additionalModelRequestFields"]["tool_choice"],
        json!({"type": "none"})
    );
    assert_json_snapshot!(w);
}

#[test]
fn gate_closed_requests_are_unchanged() {
    let plain = ToolDef::new("memory", "Memory", json!({"type": "object"}));
    let mut closed = cfg(FORCED_ID);
    closed.native_tool_types = false;
    for config in [cfg(NOVA_ID), closed] {
        let typed_req = round(vec![memory_tool(), balance_tool()]);
        let plain_req = round(vec![plain.clone(), balance_tool()]);
        let a = to_wire_json(&forced(&config, &typed_req).unwrap());
        let b = to_wire_json(&forced(&config, &plain_req).unwrap());
        assert_eq!(a, b);
    }
}

//! Integration snapshot tests for `build_request` → `to_wire_json`.
//!
//! Uses `insta::assert_json_snapshot!` so the snapshots are reviewed once and
//! then locked in as regression guards.

use insta::assert_json_snapshot;
use paigasus_helikon_core::{ContentPart, Item, ModelRequest, ResponseFormat, ToolChoice, ToolDef};
use paigasus_helikon_providers_bedrock::internal_test_helpers::{
    build_request_test, to_wire_json_test, Config,
};
use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn claude_cfg() -> Config {
    Config {
        model_id: "anthropic.claude-3-5-sonnet-20241022-v2:0".to_owned(),
    }
}

fn llama_cfg() -> Config {
    Config {
        model_id: "meta.llama3-1-70b-instruct-v1:0".to_owned(),
    }
}

fn user_text(s: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text { text: s.to_owned() }],
    }
}

// ── Snapshot tests ────────────────────────────────────────────────────────────

#[test]
fn plain_text_turn() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("hello")];
    let p = build_request_test(&claude_cfg(), &req).unwrap();
    assert_json_snapshot!(to_wire_json_test(&p));
}

#[test]
fn tool_call_and_result() {
    let mut req = ModelRequest::new();
    req.tools = vec![ToolDef {
        name: "get_balance".to_owned(),
        description: "Get account balance".to_owned(),
        schema: json!({"type": "object", "properties": {"account_id": {"type": "string"}}}),
    }];
    req.messages = vec![
        user_text("What's my balance?"),
        Item::AssistantMessage {
            content: vec![ContentPart::ToolUse {
                call_id: "tu_1".to_owned(),
                name: "get_balance".to_owned(),
                args: json!({"account_id": "acc_123"}),
            }],
            agent: None,
        },
        Item::ToolResult {
            call_id: "tu_1".to_owned(),
            content: vec![ContentPart::Text {
                text: "$1,234.56".to_owned(),
            }],
        },
    ];
    let p = build_request_test(&claude_cfg(), &req).unwrap();
    assert_json_snapshot!(to_wire_json_test(&p));
}

#[test]
fn structured_output_json_schema_on_claude_synthesizes() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("Extract the transaction data.")];
    req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Transaction".to_owned(),
        schema: json!({
            "type": "object",
            "properties": {
                "amount": {"type": "number"},
                "currency": {"type": "string"},
            },
            "required": ["amount", "currency"]
        }),
        strict: false,
    });
    let p = build_request_test(&claude_cfg(), &req).unwrap();
    assert_json_snapshot!(to_wire_json_test(&p));
}

#[test]
fn tool_choice_auto() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("hi")];
    req.tools = vec![ToolDef {
        name: "ping".to_owned(),
        description: "Ping".to_owned(),
        schema: json!({"type": "object"}),
    }];
    req.model_settings.tool_choice = Some(ToolChoice::Auto);
    let p = build_request_test(&claude_cfg(), &req).unwrap();
    assert_json_snapshot!(to_wire_json_test(&p));
}

#[test]
fn tool_choice_required() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("hi")];
    req.tools = vec![ToolDef {
        name: "ping".to_owned(),
        description: "Ping".to_owned(),
        schema: json!({"type": "object"}),
    }];
    req.model_settings.tool_choice = Some(ToolChoice::Required);
    let p = build_request_test(&claude_cfg(), &req).unwrap();
    assert_json_snapshot!(to_wire_json_test(&p));
}

#[test]
fn tool_choice_specific_tool() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("hi")];
    req.tools = vec![ToolDef {
        name: "ping".to_owned(),
        description: "Ping".to_owned(),
        schema: json!({"type": "object"}),
    }];
    req.model_settings.tool_choice = Some(ToolChoice::Tool {
        name: "ping".to_owned(),
    });
    let p = build_request_test(&claude_cfg(), &req).unwrap();
    assert_json_snapshot!(to_wire_json_test(&p));
}

#[test]
fn inference_config_temperature_top_p_max_tokens() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("hi")];
    req.model_settings.temperature = Some(0.7);
    req.model_settings.top_p = Some(0.9);
    req.model_settings.max_output_tokens = Some(256);
    let p = build_request_test(&claude_cfg(), &req).unwrap();
    assert_json_snapshot!(to_wire_json_test(&p));
}

#[test]
fn unsupported_family_json_schema_degrades_to_no_synthesis() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("classify this")];
    req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Category".to_owned(),
        schema: json!({"type": "object", "properties": {"label": {"type": "string"}}}),
        strict: false,
    });
    let p = build_request_test(&llama_cfg(), &req).unwrap();
    let wire = to_wire_json_test(&p);
    // No synthesis on Llama
    assert_eq!(wire["synthesizing"], false);
    assert_json_snapshot!(wire);
}

#[test]
fn conflict_json_schema_plus_tool_choice_tool_returns_err() {
    let mut req = ModelRequest::new();
    req.messages = vec![user_text("hi")];
    req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "X".to_owned(),
        schema: json!({}),
        strict: false,
    });
    req.model_settings.tool_choice = Some(ToolChoice::Tool {
        name: "some_tool".to_owned(),
    });
    let err = build_request_test(&claude_cfg(), &req).unwrap_err();
    assert!(matches!(err, paigasus_helikon_core::ModelError::Other(_)));
}

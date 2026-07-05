//! Round-trip serde coverage for the wire types durable runners persist.

use paigasus_helikon_core::{
    FinishReason, ModelRequest, ResponseFormat, ToolCallOutcome, ToolCallRequest, ToolChoice,
    ToolDef,
};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    serde_json::from_str(&serde_json::to_string(v).expect("serialize")).expect("deserialize")
}

#[test]
fn model_request_round_trips() {
    let mut req = ModelRequest::new();
    req.messages = vec![];
    req.tools = vec![ToolDef {
        name: "echo".into(),
        description: "d".into(),
        schema: serde_json::json!({"type": "object"}),
    }];
    req.model_settings.temperature = Some(0.2);
    req.model_settings.tool_choice = Some(ToolChoice::Required);
    req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Out".into(),
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    });

    let back = round_trip(&req);
    assert_eq!(back.tools[0].name, "echo");
    assert_eq!(back.model_settings.temperature, Some(0.2));
}

#[test]
fn tool_call_types_round_trip() {
    let call = ToolCallRequest {
        call_id: "c1".into(),
        name: "echo".into(),
        args: serde_json::json!({"x": 1}),
    };
    let outcome = ToolCallOutcome {
        call_id: "c1".into(),
        result: Err("boom".into()),
    };
    assert_eq!(round_trip(&call).call_id, "c1");
    assert!(round_trip(&outcome).result.is_err());
}

#[test]
fn finish_reason_round_trips() {
    let r: FinishReason = round_trip(&FinishReason::Other("weird".into()));
    assert_eq!(r, FinishReason::Other("weird".into()));
}

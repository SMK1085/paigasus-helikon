//! Round-trip serde coverage for the wire types durable runners persist.

use paigasus_helikon_core::{
    FinishReason, ModelRequest, OutputType, ProviderToolType, ResponseFormat, ToolCallOutcome,
    ToolCallRequest, ToolChoice, ToolDef,
};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> T {
    serde_json::from_str(&serde_json::to_string(v).expect("serialize")).expect("deserialize")
}

/// Minimal schema struct for [`OutputType::from_schema`] round-trip coverage.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    #[allow(dead_code)]
    value: u32,
}

#[test]
fn model_request_round_trips() {
    let mut req = ModelRequest::new();
    req.messages = vec![];
    req.tools = vec![ToolDef::new(
        "echo",
        "d",
        serde_json::json!({"type": "object"}),
    )];
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

/// SMA-332: `AgentPlan` (paigasus-helikon-runtime-temporal) needs
/// `OutputType: Clone + Serialize + Deserialize` so a durable driver can
/// carry a structured-output configuration. `name`/`schema` survive the
/// round-trip; the captured `validate` closure cannot cross the boundary and
/// is documented to fail closed afterward (see `OutputType`'s doc comment) —
/// assert that degraded behavior explicitly so a future accidental
/// fail-*open* regression is caught here.
#[test]
fn output_type_round_trips_name_and_schema_but_validate_fails_closed() {
    let original = OutputType::from_schema::<Answer>();
    assert!(original.validate(&serde_json::json!({"value": 7})).is_ok());

    let back = round_trip(&original);
    assert_eq!(back.name, original.name);
    assert_eq!(
        serde_json::to_value(&back.schema).unwrap(),
        serde_json::to_value(&original.schema).unwrap()
    );

    // The deserialized copy's validator is the fail-closed stand-in: even
    // conformant input is rejected.
    assert!(back.validate(&serde_json::json!({"value": 7})).is_err());
}

#[test]
fn tool_def_without_provider_types_serializes_as_before() {
    let def = ToolDef::new("echo", "d", serde_json::json!({"type": "object"}));
    assert_eq!(
        serde_json::to_value(&def).unwrap(),
        serde_json::json!({"name": "echo", "description": "d", "schema": {"type": "object"}})
    );
}

#[test]
fn old_tool_def_json_deserializes_with_empty_provider_types() {
    let def: ToolDef = serde_json::from_value(
        serde_json::json!({"name": "echo", "description": "d", "schema": {"type": "object"}}),
    )
    .unwrap();
    assert!(def.provider_types.is_empty());
}

#[test]
fn provider_tool_type_round_trips_with_type_key() {
    use serde_json::json;
    let mut params = serde_json::Map::new();
    params.insert("max_characters".into(), json!(10000));
    let def = ToolDef::new("memory", "d", json!({"type": "object"})).with_provider_types(vec![
        ProviderToolType::new("anthropic", "memory_20250818"),
        ProviderToolType::new("anthropic", "text_editor_20250728").with_params(params),
    ]);
    let v = serde_json::to_value(&def).unwrap();
    assert_eq!(
        v["provider_types"][0],
        json!({"provider": "anthropic", "type": "memory_20250818"})
    );
    // params nest under "params"
    assert_eq!(
        v["provider_types"][1]["max_characters"],
        serde_json::Value::Null
    );
    assert_eq!(
        v["provider_types"][1]["params"]["max_characters"],
        json!(10000)
    );
    let back: ToolDef = serde_json::from_value(v).unwrap();
    assert_eq!(back, def);
}

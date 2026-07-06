//! MockModel replay + script mirror tests.

use futures_util::StreamExt as _;
use paigasus_helikon_core::{CancellationToken, Model, ModelEvent, ModelRequest};
use paigasus_helikon_evals::{MockModel, ScriptFile};

const SCRIPT_JSON: &str = r#"{
  "default": [[ {"type":"token_delta","text":"hi"}, {"type":"finish","reason":"stop"} ]],
  "cases": {
    "tools": [[
      {"type":"tool_call_delta","call_id":"c1","name":"lookup_spending","args_delta":"{}"},
      {"type":"finish","reason":"tool_calls"}
    ]]
  }
}"#;

#[tokio::test]
async fn replays_script_and_exhausts() {
    let model = MockModel::with_script(vec![ModelEvent::TokenDelta {
        text: "hello".into(),
    }]);
    let mut s = model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.unwrap().unwrap();
    assert!(matches!(first, ModelEvent::TokenDelta { text } if text == "hello"));
    // second invoke: exhausted
    assert!(model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .is_err());
}

#[test]
fn script_file_selects_per_case_with_default_fallback() {
    let f: ScriptFile = serde_json::from_str(SCRIPT_JSON).unwrap();
    let tools = f.scripts_for("tools");
    assert!(
        matches!(&tools[0][0], ModelEvent::ToolCallDelta { name: Some(n), .. } if n == "lookup_spending")
    );
    let dflt = f.scripts_for("anything-else");
    assert!(matches!(&dflt[0][0], ModelEvent::TokenDelta { text } if text == "hi"));
}

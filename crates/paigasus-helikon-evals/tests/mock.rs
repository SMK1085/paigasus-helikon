//! MockModel replay + script mirror tests.

use futures_util::StreamExt as _;
use paigasus_helikon_core::{CancellationToken, FinishReason, Model, ModelEvent, ModelRequest};
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

#[tokio::test]
async fn from_script_file_replays_default_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.json");
    std::fs::write(&path, SCRIPT_JSON).unwrap();
    let model = MockModel::from_script_file(&path).unwrap();
    let mut s = model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .unwrap();
    let first = s.next().await.unwrap().unwrap();
    assert!(matches!(first, ModelEvent::TokenDelta { text } if text == "hi"));
}

#[tokio::test]
async fn from_script_file_cases_only_yields_exhausted_mock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cases-only.json");
    std::fs::write(
        &path,
        r#"{"cases":{"tools":[[{"type":"finish","reason":"stop"}]]}}"#,
    )
    .unwrap();
    let model = MockModel::from_script_file(&path).unwrap();
    // No `default` key -> empty default scripts -> first invoke errors.
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

/// The three-event script the cancellation tests share.
fn abc_script() -> Vec<ModelEvent> {
    vec![
        ModelEvent::TokenDelta { text: "a".into() },
        ModelEvent::TokenDelta { text: "b".into() },
        ModelEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

/// Guards the drop-combinator: with no cancellation the whole script must
/// still arrive, terminal `Finish` included. No other test in this repo
/// drains a `MockModel` stream to `None`, and `eval_run.rs` cannot catch a
/// dropped trailing `Finish` because `ModelTurnAccumulator` defaults
/// `finish_reason` to `Stop` — so an off-by-one here would ship green.
#[tokio::test]
async fn uncancelled_invoke_yields_full_script() {
    let model = MockModel::with_script(abc_script());
    let mut s = model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .unwrap();

    let mut got = Vec::new();
    while let Some(item) = s.next().await {
        got.push(item.unwrap());
    }

    assert_eq!(got.len(), 3, "whole script must arrive: {got:?}");
    assert!(matches!(&got[0], ModelEvent::TokenDelta { text } if text == "a"));
    assert!(matches!(&got[1], ModelEvent::TokenDelta { text } if text == "b"));
    assert!(
        matches!(
            &got[2],
            ModelEvent::Finish {
                reason: FinishReason::Stop
            }
        ),
        "terminal Finish must not be dropped: {:?}",
        got[2]
    );
}

/// The acceptance criterion: cancelling mid-stream truncates and withholds
/// `Finish`, per the `Model::invoke` contract.
#[tokio::test]
async fn cancel_mid_stream_ends_without_finish() {
    let cancel = CancellationToken::new();
    let model = MockModel::with_script(abc_script());
    let mut s = model
        .invoke(ModelRequest::new(), cancel.clone())
        .await
        .unwrap();

    let first = s.next().await.unwrap().unwrap();
    assert!(matches!(&first, ModelEvent::TokenDelta { text } if text == "a"));

    cancel.cancel();

    let rest: Vec<_> = {
        let mut v = Vec::new();
        while let Some(item) = s.next().await {
            v.push(item.unwrap());
        }
        v
    };
    assert!(
        rest.is_empty(),
        "stream must end on cancellation, got {rest:?}"
    );

    let all = [vec![first], rest].concat();
    assert!(
        !all.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
        "a cancelled stream must not emit Finish: {all:?}"
    );
}

/// A pre-cancelled `invoke` yields nothing but still consumes its script, so
/// "one script per invoke" holds regardless of cancellation timing.
///
/// Invokes #2 and #3 get FRESH tokens on purpose: reusing the cancelled one
/// would make their streams empty too and the "second script" assertion
/// unwritable.
#[tokio::test]
async fn pre_cancelled_invoke_is_empty_and_still_pops() {
    let cancel = CancellationToken::new();
    cancel.cancel();

    let model = MockModel::with_scripts(vec![
        vec![ModelEvent::TokenDelta {
            text: "first".into(),
        }],
        vec![ModelEvent::TokenDelta {
            text: "second".into(),
        }],
    ]);

    let mut s1 = model
        .invoke(ModelRequest::new(), cancel.clone())
        .await
        .unwrap();
    assert!(
        s1.next().await.is_none(),
        "a pre-cancelled invoke must yield an empty stream"
    );

    // Fresh token: proves script #1 was popped, not replayed.
    let mut s2 = model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .unwrap();
    let ev = s2.next().await.unwrap().unwrap();
    assert!(
        matches!(&ev, ModelEvent::TokenDelta { text } if text == "second"),
        "invoke #2 must get the SECOND script, got {ev:?}"
    );

    assert!(
        model
            .invoke(ModelRequest::new(), CancellationToken::new())
            .await
            .is_err(),
        "both scripts consumed, so invoke #3 must report exhaustion"
    );
}

/// Cancelling part-way through a tool call truncates the accumulated
/// `args_delta`. That is correct — it is what a real provider does when the
/// connection drops mid-call — and this pins it at the `Model` boundary. The
/// downstream consequence (core's `build_items` fails to parse the truncated
/// JSON) is documented in the spec, not asserted here.
#[tokio::test]
async fn cancel_mid_tool_call_truncates_the_args() {
    let cancel = CancellationToken::new();
    let model = MockModel::with_script(vec![
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: Some("lookup_spending".into()),
            args_delta: "{\"month\":".into(),
        },
        ModelEvent::ToolCallDelta {
            call_id: "c1".into(),
            name: None,
            args_delta: "\"july\"}".into(),
        },
        ModelEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]);
    let mut s = model
        .invoke(ModelRequest::new(), cancel.clone())
        .await
        .unwrap();

    let first = s.next().await.unwrap().unwrap();
    assert!(matches!(
        &first,
        ModelEvent::ToolCallDelta { args_delta, .. } if args_delta == "{\"month\":"
    ));

    cancel.cancel();

    let mut rest = Vec::new();
    while let Some(item) = s.next().await {
        rest.push(item.unwrap());
    }
    assert!(
        rest.is_empty(),
        "stream must end mid-tool-call on cancellation, got {rest:?}"
    );
}

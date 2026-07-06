//! CliModel construction tests (mock path only — the OpenAI/Anthropic
//! builders require an API key at build time, so those variants are not
//! constructed here).

use std::path::Path;

use futures_util::StreamExt as _;
use paigasus_helikon_cli::model::{build_model, build_model_for_case, CliModel};
use paigasus_helikon_cli::sidecar::ModelDef;
use paigasus_helikon_core::{CancellationToken, Model, ModelEvent, ModelRequest};

const SCRIPT_JSON: &str = r#"{
  "default": [[ {"type":"token_delta","text":"default-reply"}, {"type":"finish","reason":"stop"} ]],
  "cases": {
    "case-1": [[ {"type":"token_delta","text":"case-reply"}, {"type":"finish","reason":"stop"} ]]
  }
}"#;

fn mock_def(dir: &Path) -> ModelDef {
    std::fs::write(dir.join("script.json"), SCRIPT_JSON).unwrap();
    ModelDef::Mock {
        script: "script.json".into(),
    }
}

async fn first_token(model: &CliModel) -> String {
    let mut stream = model
        .invoke(ModelRequest::new(), CancellationToken::new())
        .await
        .unwrap();
    match stream.next().await.unwrap().unwrap() {
        ModelEvent::TokenDelta { text } => text,
        other => panic!("expected TokenDelta, got {other:?}"),
    }
}

#[tokio::test]
async fn build_model_mock_replays_default_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let def = mock_def(dir.path());
    let model = build_model(&def, dir.path()).unwrap();
    assert!(matches!(model, CliModel::Mock(_)));
    assert_eq!(model.provider(), "mock");
    assert_eq!(first_token(&model).await, "default-reply");
}

#[tokio::test]
async fn build_model_for_case_selects_case_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let def = mock_def(dir.path());
    let model = build_model_for_case(&def, dir.path(), "case-1").unwrap();
    assert_eq!(first_token(&model).await, "case-reply");
}

#[tokio::test]
async fn build_model_for_case_falls_back_to_default_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let def = mock_def(dir.path());
    let model = build_model_for_case(&def, dir.path(), "no-such-case").unwrap();
    assert_eq!(first_token(&model).await, "default-reply");
}

#[test]
fn build_model_missing_script_file_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let def = ModelDef::Mock {
        script: "does-not-exist.json".into(),
    };
    assert!(build_model(&def, dir.path()).is_err());
    assert!(build_model_for_case(&def, dir.path(), "case-1").is_err());
}

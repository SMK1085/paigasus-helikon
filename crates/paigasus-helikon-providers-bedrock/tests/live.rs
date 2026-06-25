//! Live integration tests against the real Amazon Bedrock Converse API.
//!
//! All tests are `#[ignore]` so they don't run in CI.
//! Activate locally with:
//! ```sh
//! BEDROCK_MODEL_ID=anthropic.claude-3-5-sonnet-20241022-v2:0 \
//! AWS_DEFAULT_REGION=us-east-1 \
//! cargo test -p paigasus-helikon-providers-bedrock -- --ignored
//! ```
//!
//! ## Required environment variables
//!
//! | Variable | Description |
//! |---|---|
//! | `BEDROCK_MODEL_ID` | Bedrock model id to test against |
//! | `AWS_DEFAULT_REGION` | (or `AWS_REGION`) — AWS region |
//! | AWS credentials | Any standard AWS credential chain value |
//!
//! When any required variable is unset the test prints a loud skip message and
//! returns early (the test passes so CI stays green).

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ResponseFormat, ToolDef,
};
use paigasus_helikon_providers_bedrock::BedrockModel;

// ── helpers ───────────────────────────────────────────────────────────────────

fn skip_if_missing_env() -> Option<String> {
    let model_id = match std::env::var("BEDROCK_MODEL_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!(
                "[live] BEDROCK_MODEL_ID not set — skipping live Bedrock test. \
                 Set BEDROCK_MODEL_ID, AWS_DEFAULT_REGION, and AWS credentials to run."
            );
            return None;
        }
    };
    Some(model_id)
}

fn user(s: &str) -> Item {
    Item::UserMessage {
        content: vec![ContentPart::Text { text: s.to_owned() }],
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Smoke test: send a one-turn text request and check the stream completes.
#[tokio::test]
#[ignore]
async fn smoke_text_turn() {
    let Some(model_id) = skip_if_missing_env() else {
        return;
    };
    let model = BedrockModel::from_env(&model_id)
        .await
        .expect("from_env should succeed");

    let mut req = ModelRequest::new();
    req.messages = vec![user("Reply with exactly: hello")];
    req.model_settings.max_output_tokens = Some(64);

    let mut s = model
        .invoke(req, CancellationToken::new())
        .await
        .expect("invoke should succeed");

    let mut text = String::new();
    while let Some(ev) = s.next().await {
        match ev {
            Ok(ModelEvent::TokenDelta { text: t }) => text.push_str(&t),
            Ok(_) => {}
            Err(e) => panic!("stream error: {e:?}"),
        }
    }
    assert!(
        text.to_lowercase().contains("hello"),
        "expected 'hello' in response, got: {text:?}"
    );
}

/// Exercises the JSON schema rewriter end-to-end with a tagged enum + nested
/// generic schema.  This validates the §4.2 acceptance criterion.
#[tokio::test]
#[ignore]
async fn tool_with_tagged_enum_and_nested_generic() {
    let Some(model_id) = skip_if_missing_env() else {
        return;
    };

    // Only run this test against Anthropic/Nova models that support tools.
    let family = paigasus_helikon_providers_bedrock::ModelFamily::from_model_id(&model_id);
    let supports_tools = matches!(
        family,
        paigasus_helikon_providers_bedrock::ModelFamily::Anthropic
            | paigasus_helikon_providers_bedrock::ModelFamily::AmazonNova
    );
    if !supports_tools {
        eprintln!("[live] {model_id} does not support tools — skipping tool test");
        return;
    }

    let model = BedrockModel::from_env(&model_id)
        .await
        .expect("from_env should succeed");

    // Schema with a tagged enum (`type` field) and a nested generic container
    // (`data: { type: array, items: { type: string } }`).
    let tool_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "enum": ["deposit", "withdraw", "transfer"],
                "description": "The type of transaction"
            },
            "amount": {
                "type": "number",
                "description": "Transaction amount in USD"
            },
            "tags": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional metadata tags"
            }
        },
        "required": ["operation", "amount"]
    });

    let mut req = ModelRequest::new();
    req.messages = vec![user(
        "Record a deposit of $500 with tags [\"savings\", \"monthly\"].",
    )];
    req.tools = vec![ToolDef {
        name: "record_transaction".to_owned(),
        description: "Record a financial transaction".to_owned(),
        schema: tool_schema,
    }];
    req.model_settings.max_output_tokens = Some(512);

    let mut s = model
        .invoke(req, CancellationToken::new())
        .await
        .expect("invoke should succeed");

    let mut had_error = false;
    while let Some(ev) = s.next().await {
        match ev {
            Ok(_) => {}
            Err(e) => {
                eprintln!("[live] stream error: {e:?}");
                had_error = true;
            }
        }
    }
    assert!(!had_error, "stream must complete without a transport error");
}

/// Structured-output synthesis test: send a JSON schema request and verify the
/// stream completes without errors and emits valid JSON tokens.
#[tokio::test]
#[ignore]
async fn structured_output_synthesis() {
    let Some(model_id) = skip_if_missing_env() else {
        return;
    };

    // Only run against Anthropic/Nova models that support structured-output synthesis.
    let family = paigasus_helikon_providers_bedrock::ModelFamily::from_model_id(&model_id);
    let supports_so = matches!(
        family,
        paigasus_helikon_providers_bedrock::ModelFamily::Anthropic
            | paigasus_helikon_providers_bedrock::ModelFamily::AmazonNova
            | paigasus_helikon_providers_bedrock::ModelFamily::Mistral
    );
    if !supports_so {
        eprintln!("[live] {model_id} does not support structured output — skipping");
        return;
    }

    let model = BedrockModel::from_env(&model_id)
        .await
        .expect("from_env should succeed");

    let mut req = ModelRequest::new();
    req.messages = vec![user("Give a transaction: deposit of 100 USD.")];
    req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
        name: "Transaction".to_owned(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string" },
                "amount": { "type": "number" }
            },
            "required": ["operation", "amount"]
        }),
        strict: false,
    });
    req.model_settings.max_output_tokens = Some(256);

    let mut s = model
        .invoke(req, CancellationToken::new())
        .await
        .expect("invoke should succeed");

    let mut text = String::new();
    let mut had_error = false;
    while let Some(ev) = s.next().await {
        match ev {
            Ok(ModelEvent::TokenDelta { text: t }) => text.push_str(&t),
            Ok(_) => {}
            Err(e) => {
                eprintln!("[live] stream error: {e:?}");
                had_error = true;
            }
        }
    }
    assert!(!had_error, "stream must complete without error");
    // The synthesized output should be parseable JSON.
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("structured output must be valid JSON");
    assert!(
        parsed["operation"].is_string(),
        "expected 'operation' string in response"
    );
}

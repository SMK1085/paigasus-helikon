//! collect_typed deserializes the terminal assistant text into T, and maps
//! a StructuredOutputFailed event to AgentError::InvalidStructuredOutput.

use futures_util::stream;
use paigasus_helikon_core::{
    AgentError, AgentEvent, ContentPart, Item, RunResultStreaming, TokenUsage,
};

#[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    #[allow(dead_code)]
    value: u32,
}

#[tokio::test]
async fn collect_typed_returns_struct() {
    let events = vec![
        AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: vec![ContentPart::Text {
                    text: "{\"value\":7}".into(),
                }],
                agent: None,
            },
        },
        AgentEvent::RunCompleted {
            usage: TokenUsage::default(),
        },
    ];
    let stream = Box::pin(stream::iter(events));
    let result = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect("collect_typed should succeed");
    assert_eq!(result.final_output, Answer { value: 7 });
}

#[tokio::test]
async fn collect_typed_maps_structured_failure() {
    let events = vec![
        AgentEvent::StructuredOutputFailed {
            schema_errors: vec!["missing field `value`".into()],
            final_text: "{}".into(),
        },
        AgentEvent::RunFailed {
            error: "invalid structured output".into(),
        },
    ];
    let stream = Box::pin(stream::iter(events));
    let err = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect_err("should be an error");
    match err {
        AgentError::InvalidStructuredOutput {
            schema_errors,
            final_text,
        } => {
            assert_eq!(schema_errors, vec!["missing field `value`".to_string()]);
            assert_eq!(final_text, "{}");
        }
        other => panic!("expected InvalidStructuredOutput, got {other:?}"),
    }
}

#[tokio::test]
async fn collect_typed_maps_plain_run_failure_to_other() {
    // A RunFailed with no preceding StructuredOutputFailed (e.g. MaxTurnsExceeded)
    // surfaces as AgentError::Other, not InvalidStructuredOutput.
    let events = vec![AgentEvent::RunFailed {
        error: "max turns (1) exceeded".into(),
    }];
    let stream = Box::pin(stream::iter(events));
    let err = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect_err("should be an error");
    match err {
        AgentError::Other(e) => assert!(e.to_string().contains("max turns")),
        other => panic!("expected Other, got {other:?}"),
    }
}

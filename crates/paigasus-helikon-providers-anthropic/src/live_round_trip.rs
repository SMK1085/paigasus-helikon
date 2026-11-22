//! Live round trip of thinking blocks. Ignored by default.
//!
//! Run: `ANTHROPIC_API_KEY=... cargo test -p paigasus-helikon-providers-anthropic -- --ignored live_`
//! Optional: `ANTHROPIC_LIVE_MODEL_ID` (default `claude-sonnet-5-5`, used with
//! `ExtendedThinking::Adaptive`). Optional: `ANTHROPIC_LIVE_ENABLED_MODEL_ID`
//! (default `claude-haiku-4-5`, used with `ExtendedThinking::Enabled`).
//!
//! Claude Sonnet 5.5 returns thinking blocks with empty text. No test here
//! needs a non-empty thinking text.
//!
//! The tests live in `src/` so that they can call `build_body`. No assertion
//! message holds a signature or a redacted value. A message holds lengths and
//! counts only.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    Agent, AgentInput, CancellationToken, ContentPart, FinishReason, Item, LlmAgent, Model,
    ModelCapabilities, ModelError, ModelEvent, ModelRequest, ModelTurn, ModelTurnAccumulator,
    RunContext, RunResultStreaming, StopReason, Tool, ToolContext, ToolDef, ToolError, ToolOutput,
};

use crate::provider_data::{from_part, AnthropicData, ThinkingBlock};
use crate::translate::build_body;
use crate::{AnthropicModel, Effort, ExtendedThinking};

/// The prompt of the probe script.
const BASE_PROMPT: &str = "What is the weather in Paris? First look up the city code, then get \
the weather with that code. Think briefly before each step.";

/// The documented string that makes Anthropic return a redacted thinking block.
const REDACT_TRIGGER: &str = "ANTHROPIC_MAGIC_STRING_TRIGGER_REDACTED_THINKING_46C9A13E193C177646C7398A98432ECCCE4C1253D5E2D82641AC0E52CC2876CB";

const INTERLEAVED_BETA: &str = "interleaved-thinking-2025-05-14";

fn has_key() -> bool {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("skipped: set ANTHROPIC_API_KEY");
        return false;
    }
    true
}

fn model_id() -> String {
    std::env::var("ANTHROPIC_LIVE_MODEL_ID").unwrap_or_else(|_| "claude-sonnet-5-5".into())
}

fn enabled_model_id() -> String {
    std::env::var("ANTHROPIC_LIVE_ENABLED_MODEL_ID").unwrap_or_else(|_| "claude-haiku-4-5".into())
}

/// The default model with adaptive thinking and high effort. At the default
/// effort `medium`, this model can skip thinking on a short task.
fn make_model(beta: Option<&str>) -> AnthropicModel {
    let mut builder = AnthropicModel::messages(model_id())
        .extended_thinking(ExtendedThinking::Adaptive)
        .effort(Effort::High);
    if let Some(beta) = beta {
        builder = builder.beta(beta);
    }
    builder.build().expect("the model builds from the env key")
}

/// A model from before 4.6 with enabled thinking. Adaptive thinking turns on
/// interleaved thinking by itself on 4.6 and later, so the interleaved beta
/// has a meaning only on this path.
fn make_enabled_model(beta: Option<&str>) -> AnthropicModel {
    let mut builder =
        AnthropicModel::messages(enabled_model_id()).extended_thinking(ExtendedThinking::Enabled {
            budget_tokens: 2048,
        });
    if let Some(beta) = beta {
        builder = builder.beta(beta);
    }
    builder.build().expect("the model builds from the env key")
}

fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "lookup_city_code",
            "Return the internal code for a city. You need the code before you can get the weather.",
            serde_json::json!({
                "type":"object","properties":{"city":{"type":"string"}},"required":["city"]
            }),
        ),
        ToolDef::new(
            "weather_by_code",
            "Get the weather for an internal city code.",
            serde_json::json!({
                "type":"object","properties":{"code":{"type":"string"}},"required":["code"]
            }),
        ),
    ]
}

fn tool_result_text(name: &str) -> &'static str {
    if name == "lookup_city_code" {
        "PAR-7351"
    } else {
        "18 C, light rain"
    }
}

fn first_request(prompt: &str) -> ModelRequest {
    let mut request = ModelRequest::new();
    request.messages = vec![Item::UserMessage {
        content: vec![ContentPart::Text {
            text: prompt.into(),
        }],
    }];
    request.tools = tool_defs();
    // Anthropic rejects a forced tool_choice with thinking on, so none is set.
    request.model_settings.max_output_tokens = Some(4096);
    request
}

/// Invoke the model and collect one turn. A stream error fails the test.
async fn run_turn(model: &dyn Model, request: ModelRequest, label: &str) -> ModelTurn {
    let mut stream = model
        .invoke(request, CancellationToken::new())
        .await
        .unwrap_or_else(|e| panic!("{label}: invoke failed: {e}"));
    let mut acc = ModelTurnAccumulator::new("root");
    while let Some(event) = stream.next().await {
        acc.observe(&event.unwrap_or_else(|e| panic!("{label}: stream error: {e}")));
    }
    acc.finish()
        .unwrap_or_else(|e| panic!("{label}: the turn does not assemble: {e}"))
}

/// The thinking blocks of the parts of the items, in order.
fn blocks_of_parts<'a>(parts: impl Iterator<Item = &'a ContentPart>) -> Vec<ThinkingBlock> {
    parts
        .filter_map(|part| match from_part(part) {
            Some((AnthropicData::Block { block, .. }, _)) => Some(block),
            _ => None,
        })
        .collect()
}

fn blocks_of_items(items: &[Item]) -> Vec<ThinkingBlock> {
    blocks_of_parts(items.iter().flat_map(|item| match item {
        Item::AssistantMessage { content, .. } => content.as_slice(),
        _ => &[],
    }))
}

fn signatures(blocks: &[ThinkingBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ThinkingBlock::Thinking { signature, .. } => Some(signature.clone()),
            ThinkingBlock::RedactedThinking { .. } => None,
        })
        .collect()
}

fn redacted_data(blocks: &[ThinkingBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            ThinkingBlock::RedactedThinking { data } => Some(data.clone()),
            ThinkingBlock::Thinking { .. } => None,
        })
        .collect()
}

fn tool_calls(items: &[Item]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::ToolCall { call_id, name, .. } => Some((call_id.clone(), name.clone())),
            _ => None,
        })
        .collect()
}

/// The turn 2 request: the messages of turn 1, then one result per call.
fn second_request(first: &ModelRequest, items: &[Item]) -> ModelRequest {
    let mut request = first.clone();
    request.messages.extend(items.iter().cloned());
    for (call_id, name) in tool_calls(items) {
        request.messages.push(Item::ToolResult {
            call_id,
            content: vec![ContentPart::Text {
                text: tool_result_text(&name).into(),
            }],
        });
    }
    request
}

fn assert_turn_ok(turn: &ModelTurn, label: &str) {
    assert!(
        matches!(
            turn.finish_reason,
            FinishReason::Stop | FinishReason::ToolCalls
        ),
        "{label}: finish is not Stop or ToolCalls: {:?}",
        turn.finish_reason
    );
}

/// Turn 1, the check of the turn 2 body, and turn 2. Returns the turn 2 items.
async fn thinking_round_trip(model: &AnthropicModel) -> Vec<Item> {
    let first = first_request(BASE_PROMPT);
    let turn1 = run_turn(model, first.clone(), "turn 1").await;
    assert!(
        !tool_calls(&turn1.items).is_empty(),
        "turn 1 has no tool call ({} items); the model did not call a tool",
        turn1.items.len()
    );
    let sigs = signatures(&blocks_of_items(&turn1.items));
    assert!(
        !sigs.is_empty(),
        "turn 1 has no thinking Block part with a signature ({} items)",
        turn1.items.len()
    );

    let second = second_request(&first, &turn1.items);
    let body = build_body(&model.cfg, &second)
        .expect("the turn 2 request translates")
        .body
        .to_string();
    for sig in &sigs {
        assert!(
            body.contains(sig.as_str()),
            "the turn 2 body does not hold a signature of turn 1 (length {})",
            sig.len()
        );
    }

    let turn2 = run_turn(model, second, "turn 2").await;
    assert_turn_ok(&turn2, "turn 2");
    turn2.items
}

#[tokio::test]
#[ignore]
async fn live_thinking_round_trip() {
    if !has_key() {
        return;
    }
    thinking_round_trip(&make_model(None)).await;
}

#[tokio::test]
#[ignore]
async fn live_thinking_round_trip_enabled() {
    if !has_key() {
        return;
    }
    thinking_round_trip(&make_enabled_model(None)).await;
}

/// A hand-off changes `system` between the two requests. The prefix check
/// would reject the thinking block. With `drop_block` the API drops it and
/// the request succeeds.
#[tokio::test]
#[ignore]
async fn live_changed_system_drops_block_and_succeeds() {
    if !has_key() {
        return;
    }
    let model = make_model(None);
    let system = |text: &str| Item::System {
        content: vec![ContentPart::Text { text: text.into() }],
    };
    let mut first = first_request(BASE_PROMPT);
    first
        .messages
        .insert(0, system("You are the first agent. Answer briefly."));
    let turn1 = run_turn(&model, first.clone(), "turn 1").await;
    assert!(
        !tool_calls(&turn1.items).is_empty(),
        "turn 1 has no tool call ({} items); the model did not call a tool",
        turn1.items.len()
    );

    let sigs = signatures(&blocks_of_items(&turn1.items));
    assert!(
        !sigs.is_empty(),
        "turn 1 has no thinking Block part with a signature ({} items)",
        turn1.items.len()
    );

    let mut second = second_request(&first, &turn1.items);
    second.messages[0] = system("You are the second agent. Answer in one sentence.");
    let turn2 = run_turn(&model, second, "turn 2 with a changed system").await;
    assert_turn_ok(&turn2, "turn 2 with a changed system");
}

/// A model that records each request and delegates to Anthropic.
struct RecordingModel {
    inner: Arc<AnthropicModel>,
    seen: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl Model for RecordingModel {
    async fn invoke(
        &self,
        mut request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        request.model_settings.max_output_tokens = Some(4096);
        self.seen.lock().unwrap().push(request.clone());
        self.inner.invoke(request, cancel).await
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn provider(&self) -> &str {
        self.inner.provider()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }
}

/// A tool with a fixed name and a fixed result.
struct FixedTool {
    name: &'static str,
    description: &'static str,
    schema: serde_json::Value,
}

#[async_trait]
impl Tool<()> for FixedTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.description
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    async fn invoke(
        &self,
        _ctx: &ToolContext<()>,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(serde_json::json!(tool_result_text(
            self.name
        ))))
    }
}

fn fixed_tools() -> Vec<FixedTool> {
    tool_defs()
        .into_iter()
        .map(|def| FixedTool {
            name: if def.name == "lookup_city_code" {
                "lookup_city_code"
            } else {
                "weather_by_code"
            },
            description: if def.name == "lookup_city_code" {
                "Return the internal code for a city. You need the code before you can get the weather."
            } else {
                "Get the weather for an internal city code."
            },
            schema: def.schema.clone(),
        })
        .collect()
}

#[tokio::test]
#[ignore]
async fn live_agent_loop_sends_signature() {
    if !has_key() {
        return;
    }
    let inner = Arc::new(make_model(None));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recording = RecordingModel {
        inner: Arc::clone(&inner),
        seen: Arc::clone(&seen),
    };
    let mut builder = LlmAgent::builder::<()>()
        .name("root")
        .instructions(BASE_PROMPT)
        .model(recording);
    for tool in fixed_tools() {
        builder = builder.tool(tool);
    }
    let agent = builder.build();
    let stream = agent
        .run(
            RunContext::ephemeral(()),
            AgentInput::from_user_text("Start."),
        )
        .await
        .expect("the agent starts");
    let result = RunResultStreaming::new(stream)
        .collect()
        .await
        .expect("the run collects");
    assert!(
        matches!(result.stop_reason, StopReason::Completed),
        "the run did not complete: {:?}",
        result.stop_reason
    );

    let requests = seen.lock().unwrap().clone();
    assert!(
        requests.len() >= 2,
        "the run made {} model request(s); a tool round needs at least 2",
        requests.len()
    );
    let second = &requests[1];
    let sigs = signatures(&blocks_of_parts(second.messages.iter().flat_map(
        |item| match item {
            Item::AssistantMessage { content, .. } => content.as_slice(),
            _ => &[],
        },
    )));
    assert!(
        !sigs.is_empty(),
        "the second request has no thinking Block part with a signature"
    );
    let body = build_body(&inner.cfg, second)
        .expect("the second request translates")
        .body
        .to_string();
    for sig in &sigs {
        assert!(
            body.contains(sig.as_str()),
            "the second body does not hold a signature of its Block part (length {})",
            sig.len()
        );
    }
}

#[tokio::test]
#[ignore]
async fn live_redacted_round_trip() {
    if !has_key() {
        return;
    }
    let model = make_model(None);
    let first = first_request(&format!("{REDACT_TRIGGER} {BASE_PROMPT}"));
    let turn1 = run_turn(&model, first.clone(), "turn 1").await;
    let redacted = redacted_data(&blocks_of_items(&turn1.items));
    assert!(
        !redacted.is_empty(),
        "turn 1 has no redacted thinking block ({} items)",
        turn1.items.len()
    );
    assert!(
        !tool_calls(&turn1.items).is_empty(),
        "turn 1 has no tool call ({} items); the model did not call a tool",
        turn1.items.len()
    );

    let second = second_request(&first, &turn1.items);
    let body = build_body(&model.cfg, &second)
        .expect("the turn 2 request translates")
        .body
        .to_string();
    for data in &redacted {
        assert!(
            body.contains(data.as_str()),
            "the turn 2 body does not hold a redacted value of turn 1 (length {})",
            data.len()
        );
    }
    let turn2 = run_turn(&model, second, "turn 2").await;
    assert_turn_ok(&turn2, "turn 2");
}

#[tokio::test]
#[ignore]
async fn live_interleaved_beta_thinks_again() {
    if !has_key() {
        return;
    }
    let items = thinking_round_trip(&make_enabled_model(Some(INTERLEAVED_BETA))).await;
    assert!(
        !blocks_of_items(&items).is_empty(),
        "turn 2 has no thinking Block part. Run B saw one in turn 2 once, \
         and the model can skip thinking by chance, so run this test again"
    );
}

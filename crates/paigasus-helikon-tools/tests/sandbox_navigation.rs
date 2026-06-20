#![cfg(unix)]
#![allow(missing_docs)]

mod common;

use common::ScriptedModel;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    Agent, AgentEvent, AgentInput, ContentPart, FinishReason, Item, LlmAgent, ModelEvent,
    RunContext,
};
use paigasus_helikon_tools::{BashTool, HostBackend, ReadTool, Sandbox};

#[tokio::test]
async fn agent_navigates_sandbox_and_reports_contents() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "hello sandbox").unwrap();
    let sandbox = Sandbox::open(tmp.path()).unwrap();

    // Script: turn 0 -> Bash `ls`; turn 1 -> Read `notes.txt`; turn 2 -> answer.
    let model = ScriptedModel::new(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("Bash".into()),
                args_delta: "{\"command\":\"ls\"}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c2".into(),
                name: Some("Read".into()),
                args_delta: "{\"path\":\"notes.txt\"}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "The sandbox contains notes.txt which says: hello sandbox".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);

    let agent = LlmAgent::builder::<()>()
        .name("sandbox-explorer")
        .model(model)
        .instructions("Use the tools to inspect the sandbox, then answer.")
        .tool(ReadTool::<()>::new(sandbox.clone()))
        .tool(BashTool::<()>::new(HostBackend::builder(sandbox).build()))
        .build();

    let ctx: RunContext<()> = RunContext::ephemeral(());

    let mut stream = agent
        .run(ctx, AgentInput::from_user_text("What's in the sandbox?"))
        .await
        .expect("run starts");

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    // (1) Final assistant text mentions the file contents.
    let answered = events
        .iter()
        .any(|e| matches!(e, AgentEvent::TokenDelta { text } if text.contains("hello sandbox")));
    assert!(answered, "agent should answer with the file contents");

    // (2) ReadTool genuinely read the file: a ToolOutputItem event carries its
    // real output ("hello sandbox"), which is NOT in any tool-call args.
    let read_happened = events.iter().any(|e| match e {
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult { content, .. },
        } => content.iter().any(|part| match part {
            ContentPart::Text { text } => text.contains("hello sandbox"),
            _ => false,
        }),
        _ => false,
    });
    assert!(
        read_happened,
        "ReadTool should have returned the file's bytes"
    );
}

#[tokio::test]
async fn agent_surfaces_denied_tool_result() {
    let tmp = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::open(tmp.path()).unwrap();

    // Turn 0: model tries to read OUTSIDE the sandbox (path escape) -> Denied.
    // Turn 1: model gives up and answers.
    let model = ScriptedModel::new(vec![
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("Read".into()),
                args_delta: "{\"path\":\"../escape.txt\"}".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            ModelEvent::TokenDelta {
                text: "I could not read that file.".into(),
            },
            ModelEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]);

    let agent = LlmAgent::builder::<()>()
        .name("denial-demo")
        .model(model)
        .tool(ReadTool::<()>::new(sandbox))
        .build();

    let ctx: RunContext<()> = RunContext::ephemeral(());

    let mut stream = agent
        .run(ctx, AgentInput::from_user_text("read ../escape.txt"))
        .await
        .expect("run starts");

    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }

    // The denial reason ("operation denied: ...") must surface as a tool result
    // through the runner — proving Denied propagates end-to-end, not just via a
    // direct invoke(). The runner converts Err(e.to_string()) into
    // AgentEvent::ToolOutputItem { item: Item::ToolResult { content, .. } }
    // with a single ContentPart::Text carrying the error string.
    let denial_surfaced = events.iter().any(|e| match e {
        AgentEvent::ToolOutputItem {
            item: Item::ToolResult { content, .. },
        } => content.iter().any(|part| match part {
            ContentPart::Text { text } => text.to_lowercase().contains("denied"),
            _ => false,
        }),
        _ => false,
    });
    assert!(
        denial_surfaced,
        "the Denied error must surface as a tool result via the agent loop; events: {events:#?}"
    );
}

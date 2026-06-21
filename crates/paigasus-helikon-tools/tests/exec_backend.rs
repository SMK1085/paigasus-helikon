#![allow(missing_docs)]

use paigasus_helikon_tools::{
    ExecOutput, ExecRequest, Isolation, ResourceLimits, SandboxGuarantees,
};

#[test]
fn exec_request_new_sets_command() {
    let req = ExecRequest::new("ls -la");
    assert_eq!(req.command, "ls -la");
}

#[test]
fn resource_limits_default_is_all_none() {
    let l = ResourceLimits::default();
    assert_eq!(l.cpu_seconds, None);
    assert_eq!(l.file_size_bytes, None);
    assert_eq!(l.address_space_bytes, None);
}

#[test]
fn guarantees_struct_holds_axes_and_label() {
    let g = SandboxGuarantees::new(
        Isolation::OsKernel,
        Isolation::None,
        Isolation::OsKernel,
        "demo",
    );
    assert_eq!(g.filesystem, Isolation::OsKernel);
    assert_eq!(g.label, "demo");
    // ExecOutput is constructible and Clone.
    let o = ExecOutput::new("out".into(), String::new(), Some(0), false, false);
    assert_eq!(o.clone().stdout, "out");
}

use async_trait::async_trait;
use paigasus_helikon_core::{
    CancellationToken, HookRegistry, MemorySession, RunContext, Tool, ToolError, TracerHandle,
};
use paigasus_helikon_tools::{BashTool, ExecutionBackend};
use std::sync::Arc;

/// A backend that records the command and returns a canned output — proves
/// BashTool calls `run` and maps the result, with no real process.
struct MockBackend {
    seen: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl ExecutionBackend for MockBackend {
    async fn run(
        &self,
        req: paigasus_helikon_tools::ExecRequest,
    ) -> Result<paigasus_helikon_tools::ExecOutput, ToolError> {
        self.seen.lock().unwrap().push(req.command.clone());
        // `ExecOutput`/`SandboxGuarantees` are `#[non_exhaustive]`, so an
        // integration test (separate crate) must use the `::new(..)` constructors,
        // not struct literals.
        Ok(paigasus_helikon_tools::ExecOutput::new(
            "mocked".to_string(),
            String::new(),
            Some(0),
            false,
            false,
        ))
    }
    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees::new(
            Isolation::OsKernel,
            Isolation::OsKernel,
            Isolation::OsKernel,
            "mock",
        )
    }
}

fn tool_ctx() -> paigasus_helikon_core::ToolContext<()> {
    RunContext::<()>::new(
        Arc::new(()),
        Arc::new(MemorySession::new()),
        HookRegistry::new(),
        TracerHandle::default(),
        CancellationToken::new(),
    )
    .to_tool_context()
}

#[tokio::test]
async fn bashtool_delegates_to_any_backend_unchanged() {
    let backend = Arc::new(MockBackend {
        seen: Default::default(),
    });
    let tool: BashTool = BashTool::new(backend.clone());
    let out = tool
        .invoke(&tool_ctx(), serde_json::json!({ "command": "echo hi" }))
        .await
        .unwrap();
    assert_eq!(out.content["stdout"], "mocked");
    // All ExecOutput fields are projected into the tool output, not just stdout.
    assert_eq!(out.content["exit_code"], 0);
    assert_eq!(out.content["timed_out"], false);
    assert_eq!(out.content["truncated"], false);
    assert_eq!(backend.seen.lock().unwrap().as_slice(), ["echo hi"]);
    // The backend's containment label is surfaced in the tool description.
    assert!(tool.description().contains("mock"));
}

#[test]
fn isolation_has_virtualized_variant() {
    // Virtualized is a distinct, stronger tier than OsKernel.
    let g = SandboxGuarantees::new(
        Isolation::Virtualized,
        Isolation::None,
        Isolation::Virtualized,
        "vm",
    );
    assert_eq!(g.filesystem, Isolation::Virtualized);
    assert_eq!(g.syscalls, Isolation::Virtualized);
    assert_ne!(Isolation::Virtualized, Isolation::OsKernel);
}

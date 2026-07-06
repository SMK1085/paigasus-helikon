//! `AgentRegistry` tests: build-from-sidecar, hot reload, and the file
//! watcher (SMA-333).

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_cli::registry::AgentRegistry;
use paigasus_helikon_core::RunContext;

const MOCK_SCRIPT: &str = r#"{
  "default": [[ {"type":"token_delta","text":"hi"}, {"type":"finish","reason":"stop"} ]]
}"#;

const SIDECAR_ROUTE: &str = r#"
[agents.triage]
description  = "Routes questions"
instructions = "Route the question."
model        = { provider = "mock", script = "triage.json" }
tools        = ["lookup"]
handoffs     = ["billing"]

[agents.billing]
instructions = "Handle billing."
model        = { provider = "mock", script = "billing.json" }

[tools.lookup]
description = "Look something up"
params      = { type = "object", properties = { q = { type = "string" } }, required = ["q"] }
inline      = "fn run(args) { #{ q: args.q } }"
"#;

const SIDECAR_ESCALATE: &str = r#"
[agents.triage]
description  = "Routes questions"
instructions = "Escalate the question."
model        = { provider = "mock", script = "triage.json" }
tools        = ["lookup"]
handoffs     = ["billing"]

[agents.billing]
instructions = "Handle billing."
model        = { provider = "mock", script = "billing.json" }

[tools.lookup]
description = "Look something up"
params      = { type = "object", properties = { q = { type = "string" } }, required = ["q"] }
inline      = "fn run(args) { #{ q: args.q } }"
"#;

const SIDECAR_BROKEN: &str = "this is not valid toml [[[";

/// Writes a fresh tempdir with `agents.toml` (given contents) plus the two
/// mock model scripts every fixture above references.
fn write_sidecar(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    std::fs::write(dir.join("triage.json"), MOCK_SCRIPT).unwrap();
    std::fs::write(dir.join("billing.json"), MOCK_SCRIPT).unwrap();
    let path = dir.join("agents.toml");
    std::fs::write(&path, contents).unwrap();
    path
}

#[test]
fn load_and_build_agent_walks_handoffs() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_sidecar(dir.path(), SIDECAR_ROUTE);

    let registry = AgentRegistry::load(&path).expect("load must succeed");
    assert_eq!(registry.agent_names(), vec!["billing", "triage"]);
    assert!(registry.has_agent("triage"));
    assert!(!registry.has_agent("ghost"));

    let agent = registry.build_agent("triage").expect("build must succeed");
    assert_eq!(agent.name, "triage");
    assert_eq!(agent.tools.len(), 1);
    assert_eq!(agent.handoffs.len(), 1);
    assert_eq!(agent.handoffs[0].agent().name(), "billing");
}

#[test]
fn reload_picks_up_new_instructions() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_sidecar(dir.path(), SIDECAR_ROUTE);

    let registry = AgentRegistry::load(&path).expect("load must succeed");
    let agent = registry.build_agent("triage").expect("build must succeed");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert!(agent.instructions.render(&ctx).contains("Route"));

    std::fs::write(&path, SIDECAR_ESCALATE).unwrap();
    registry.reload().expect("reload must succeed");

    let agent = registry
        .build_agent("triage")
        .expect("build after reload must succeed");
    assert!(agent.instructions.render(&ctx).contains("Escalate"));
}

#[test]
fn reload_with_broken_toml_keeps_old_defs() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_sidecar(dir.path(), SIDECAR_ROUTE);

    let registry = AgentRegistry::load(&path).expect("load must succeed");

    std::fs::write(&path, SIDECAR_BROKEN).unwrap();
    let err = registry.reload().expect_err("broken TOML must error");
    assert!(!err.to_string().is_empty());

    // Old definitions must still be usable after a failed reload.
    let agent = registry
        .build_agent("triage")
        .expect("old defs must still build after failed reload");
    assert_eq!(agent.name, "triage");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert!(agent.instructions.render(&ctx).contains("Route"));
}

#[test]
fn watch_notifies_on_file_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_sidecar(dir.path(), SIDECAR_ROUTE);

    let registry = Arc::new(AgentRegistry::load(&path).expect("load must succeed"));
    let (tx, rx) = mpsc::channel::<anyhow::Result<()>>();
    let _debouncer = registry
        .watch(move |outcome| {
            let _ = tx.send(outcome);
        })
        .expect("watch must start");

    std::fs::write(&path, SIDECAR_ESCALATE).unwrap();

    let outcome = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("watcher must notify within 10s");
    outcome.expect("reload triggered by the watcher must succeed");

    let agent = registry
        .build_agent("triage")
        .expect("build after watcher reload must succeed");
    let ctx: RunContext<()> = RunContext::ephemeral(());
    assert!(agent.instructions.render(&ctx).contains("Escalate"));
}

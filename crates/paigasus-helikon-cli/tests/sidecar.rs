//! Tests for `agents.toml` sidecar parsing + validation (SMA-333).

use std::path::Path;

use paigasus_helikon_cli::sidecar::{InstructionsDef, ModelDef, Sidecar};

const KNOWN_GOOD: &str = r#"
[agents.triage]
description  = "Routes personal-finance questions"
instructions = "Route the question."
model        = { provider = "mock", script = "triage_script.json" }
max_turns    = 8
tools        = ["lookup_spending"]
handoffs     = ["budgeting"]

[agents.budgeting]
instructions = "Answer budget questions."
model        = { provider = "openai", id = "gpt-5-mini" }

[tools.lookup_spending]
description = "Look up spending for a month"
params      = { type = "object", properties = { month = { type = "string" } }, required = ["month"] }
inline      = "fn run(args) { #{ month: args.month, total: 1250 } }"

[eval]
evaluators = ["exact_match", "tool_trajectory"]
"#;

fn base_dir() -> &'static Path {
    Path::new(".")
}

#[test]
fn fixture_parses_correctly() {
    let sidecar = Sidecar::parse(KNOWN_GOOD, base_dir()).expect("known-good fixture must parse");

    assert_eq!(sidecar.agents.len(), 2);
    assert_eq!(sidecar.tools.len(), 1);
    assert_eq!(sidecar.base_dir, base_dir());
    assert_eq!(sidecar.first_agent(), Some("budgeting")); // BTreeMap sorts keys

    let triage = sidecar.agents.get("triage").expect("triage agent present");
    assert_eq!(
        triage.description.as_deref(),
        Some("Routes personal-finance questions")
    );
    match &triage.instructions {
        InstructionsDef::Inline(text) => assert_eq!(text, "Route the question."),
        other => panic!("expected inline instructions, got {other:?}"),
    }
    match &triage.model {
        ModelDef::Mock { script } => assert_eq!(script, Path::new("triage_script.json")),
        other => panic!("expected mock model, got {other:?}"),
    }
    assert_eq!(triage.max_turns, Some(8));
    assert_eq!(triage.tools, vec!["lookup_spending".to_string()]);
    assert_eq!(triage.handoffs, vec!["budgeting".to_string()]);

    let budgeting = sidecar
        .agents
        .get("budgeting")
        .expect("budgeting agent present");
    assert_eq!(budgeting.description, None);
    assert!(budgeting.tools.is_empty());
    assert!(budgeting.handoffs.is_empty());
    match &budgeting.model {
        ModelDef::Openai { id } => assert_eq!(id, "gpt-5-mini"),
        other => panic!("expected openai model, got {other:?}"),
    }

    let tool = sidecar
        .tools
        .get("lookup_spending")
        .expect("lookup_spending tool present");
    assert_eq!(tool.description, "Look up spending for a month");
    assert!(tool.params.is_object());
    assert_eq!(tool.params["type"], "object");
    assert_eq!(tool.params["properties"]["month"]["type"], "string");
    assert_eq!(tool.params["required"][0], "month");
    assert!(tool.script.is_none());
    assert!(tool.inline.is_some());

    let eval = sidecar.eval.as_ref().expect("eval section present");
    assert_eq!(eval.evaluators, vec!["exact_match", "tool_trajectory"]);
    assert!(eval.json_schema.is_none());
    assert!(eval.llm_judge.is_none());
}

#[test]
fn file_instructions_parse_to_file_variant() {
    let toml = r#"
[agents.triage]
instructions = { file = "instructions.md" }
model        = { provider = "anthropic", id = "claude-sonnet-4-5" }
"#;
    let sidecar = Sidecar::parse(toml, base_dir()).expect("file-instructions fixture must parse");
    let triage = sidecar.agents.get("triage").expect("triage agent present");
    match &triage.instructions {
        InstructionsDef::File { file } => {
            // Stored exactly as declared — resolution against base_dir happens at use time.
            assert_eq!(file, Path::new("instructions.md"));
        }
        other => panic!("expected file instructions, got {other:?}"),
    }
    match &triage.model {
        ModelDef::Anthropic { id } => assert_eq!(id, "claude-sonnet-4-5"),
        other => panic!("expected anthropic model, got {other:?}"),
    }
}

#[test]
fn unknown_tool_ref_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }
tools        = ["does_not_exist"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("triage"), "{msg}");
    assert!(msg.contains("does_not_exist"), "{msg}");
}

#[test]
fn unknown_handoff_ref_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["ghost"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("triage"), "{msg}");
    assert!(msg.contains("ghost"), "{msg}");
}

#[test]
fn self_handoff_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["triage"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("triage"), "{msg}");
}

#[test]
fn handoff_cycle_errors() {
    let toml = r#"
[agents.a]
instructions = "A."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["b"]

[agents.b]
instructions = "B."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["a"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("handoff cycle detected involving")
            && msg.contains("declare one-way handoff chains"),
        "{msg}"
    );
}

#[test]
fn three_node_handoff_cycle_errors() {
    let toml = r#"
[agents.a]
instructions = "A."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["b"]

[agents.b]
instructions = "B."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["c"]

[agents.c]
instructions = "C."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["a"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("handoff cycle detected involving")
            && msg.contains("declare one-way handoff chains"),
        "{msg}"
    );
}

#[test]
fn diamond_handoffs_without_cycle_validate_ok() {
    // c is reachable from both a and b; the shared sink must not be
    // misreported as a cycle (pins the DFS visited-vs-stack interplay).
    let toml = r#"
[agents.a]
instructions = "A."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["c"]

[agents.b]
instructions = "B."
model        = { provider = "mock", script = "s.json" }
handoffs     = ["c"]

[agents.c]
instructions = "C."
model        = { provider = "mock", script = "s.json" }
"#;
    let sidecar = Sidecar::parse(toml, base_dir()).expect("acyclic diamond must validate");
    assert_eq!(sidecar.agents.len(), 3);
}

#[test]
fn tool_with_both_script_and_inline_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }

[tools.dupe]
description = "dupe"
params      = { type = "object" }
script      = "dupe.json"
inline      = "fn run(args) { args }"
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("dupe"), "{msg}");
}

#[test]
fn tool_with_neither_script_nor_inline_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }

[tools.empty]
description = "empty"
params      = { type = "object" }
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("empty"), "{msg}");
}

#[test]
fn non_object_params_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }

[tools.bad_params]
description = "bad"
params      = "not an object"
inline      = "fn run(args) { args }"
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("bad_params"), "{msg}");
}

#[test]
fn unknown_evaluator_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }

[eval]
evaluators = ["not_a_real_evaluator"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("not_a_real_evaluator"), "{msg}");
}

#[test]
fn named_json_schema_evaluator_missing_config_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }

[eval]
evaluators = ["json_schema"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("json_schema"), "{msg}");
}

#[test]
fn named_llm_judge_evaluator_missing_config_errors() {
    let toml = r#"
[agents.triage]
instructions = "Route."
model        = { provider = "mock", script = "s.json" }

[eval]
evaluators = ["llm_judge"]
"#;
    let err = Sidecar::parse(toml, base_dir()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("llm_judge"), "{msg}");
}

#[test]
fn load_reads_file_and_sets_base_dir_from_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agents.toml");
    std::fs::write(&path, KNOWN_GOOD).expect("write fixture");

    let sidecar = Sidecar::load(&path).expect("load must succeed");
    assert_eq!(sidecar.base_dir, dir.path());
    assert_eq!(sidecar.agents.len(), 2);
}

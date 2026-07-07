//! AC1: `helikon eval run triage.jsonl --agent triage` produces
//! trajectory + final-response scores in CI (mock provider, cwd at the
//! fixture dir so the `./agents.toml` default engages).

use std::path::Path;
use std::process::Command;

fn fixtures() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
}

#[test]
fn eval_run_scores_pass_and_exit_zero() {
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(fixtures())
        .args(["eval", "run", "triage.jsonl", "--agent", "triage"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("exact_match"),
        "final-response scores present:\n{stdout}"
    );
    assert!(
        stdout.contains("tool_trajectory"),
        "trajectory scores present:\n{stdout}"
    );
    assert!(stdout.contains("2 passed"), "summary present:\n{stdout}");
}

#[test]
fn eval_run_wrong_expectation_exits_nonzero() {
    // same fixtures, but a dataset expecting the wrong answer
    let dir = tempfile::tempdir().unwrap();
    let dataset = dir.path().join("bad.jsonl");
    std::fs::write(
        &dataset,
        r#"{"id":"greeting","input":"Hi!","expected":"WRONG"}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(fixtures())
        .args(["eval", "run"])
        .arg(&dataset)
        .args(["--agent", "triage"])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn eval_run_missing_tool_script_fails_cleanly_without_panic() {
    // Sidecar validation is filesystem-blind: a tool `script` pointing at a
    // missing .rhai file passes load-time validation and only surfaces when
    // the agent is built. The pre-flight build must turn that into the
    // CLI's normal `error: ...` path, not a mid-run panic.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("agents.toml"),
        r#"
[agents.broken]
instructions = "Test."
model        = { provider = "mock", script = "script.json" }
tools        = ["missing_tool"]

[tools.missing_tool]
description = "A tool whose script file does not exist"
params      = { type = "object" }
script      = "does_not_exist.rhai"

[eval]
evaluators = ["exact_match"]
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("script.json"),
        r#"{"default":[[{"type":"token_delta","text":"hi"},{"type":"finish","reason":"stop"}]]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("dataset.jsonl"),
        r#"{"id":"c1","input":"Hi!","expected":"hi"}"#,
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(dir.path())
        .args(["eval", "run", "dataset.jsonl", "--agent", "broken"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "stderr:\n{stderr}");
    assert!(
        !stderr.contains("panicked"),
        "must fail via the error path, not a panic:\n{stderr}"
    );
    assert!(stderr.contains("error:"), "stderr:\n{stderr}");
    assert!(
        stderr.contains("broken"),
        "error names the agent:\n{stderr}"
    );
}

#[test]
fn eval_run_json_output_parses() {
    let out = Command::new(env!("CARGO_BIN_EXE_helikon"))
        .current_dir(fixtures())
        .args(["eval", "run", "triage.jsonl", "--agent", "triage", "--json"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["summary"]["cases_passed"], 2);
}

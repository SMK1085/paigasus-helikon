//! Binary smoke tests.

#[test]
fn help_lists_subcommands() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_helikon"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for cmd in ["repl", "eval", "mcp"] {
        assert!(stdout.contains(cmd), "missing {cmd} in help");
    }
}

#[test]
fn shim_binary_works_too() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_paigasus-helikon"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(out.status.success());
}

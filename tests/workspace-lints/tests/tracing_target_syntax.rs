//! No `tracing` macro anywhere in the workspace may pass `target` or `parent`
//! with `=` instead of `:` — the `=` form silently records an ordinary field
//! and leaves the event on its module-path target (SMA-543).

use std::path::{Path, PathBuf};

use paigasus_helikon_workspace_lints::scan;

/// Repo root, derived from this crate's manifest directory rather than the
/// process CWD so it survives the member being moved.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root must resolve from CARGO_MANIFEST_DIR")
}

/// Collect `.rs` files under `dir`, skipping build output.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name == "target" || name == ".git" {
                continue;
            }
            collect_rs(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_tracing_macro_passes_target_or_parent_with_equals() {
    let root = repo_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "resolved repo root {root:?} has no Cargo.toml"
    );

    // Only the two directories that hold workspace members. Deliberately NOT
    // the repo root: `.claude/worktrees/` can hold full checkouts of other
    // branches, and scanning those would make this test's verdict depend on
    // which unrelated worktrees a developer happens to have.
    let mut files = Vec::new();
    for sub in ["crates", "tests"] {
        let dir = root.join(sub);
        assert!(dir.is_dir(), "expected workspace directory {dir:?}");
        collect_rs(&dir, &mut files);
    }

    // Anti-vacuity: a walk that finds nothing must fail, not pass. Without
    // this, a wrong root or a moved directory reports identically to a clean
    // workspace. Same reasoning as the vacuous-pass guard in
    // `crates/paigasus-helikon/tests/openai_litellm_message_parity.rs`.
    assert!(
        files.len() >= 300,
        "scanned only {} .rs files — the walk is not reaching the workspace",
        files.len()
    );
    for required in [
        "crates/paigasus-helikon-providers-openai/src/translate/request.rs",
        "crates/paigasus-helikon-providers-litellm/src/translate/request.rs",
        // Proves the non-`crates/` root is live: this member sits outside
        // `crates/` and an earlier draft of the walk would have missed it.
        "tests/runtime-http-conformance/src/lib.rs",
    ] {
        assert!(
            files.iter().any(|f| f.ends_with(required)),
            "{required} was not among the {} files scanned",
            files.len()
        );
    }

    let mut offenses = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let rel = file.strip_prefix(&root).unwrap_or(file);
        for o in scan(&src) {
            offenses.push(format!(
                "{}:{} — {}! passes `{} =`; it must be `{}:`",
                rel.display(),
                o.line,
                o.macro_name,
                o.keyword,
                o.keyword
            ));
        }
    }

    assert!(
        offenses.is_empty(),
        "{} tracing macro call site(s) use `=` where the macro requires `:`:\n{}",
        offenses.len(),
        offenses.join("\n")
    );
}

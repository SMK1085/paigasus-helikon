//! Every wire-format fixture carries a provenance header (SMA-845).
//!
//! The rule is in `CONTRIBUTING.md`, section "Wire-format fixtures".

use std::path::{Path, PathBuf};

use paigasus_helikon_workspace_lints::fixture_header::check;

/// Fixtures that do not yet pass `check`, as repo-relative paths.
///
/// This list may only shrink. Phase B of SMA-845 empties it.
const ALLOWLIST: &[&str] = &[
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/body_cut_inside_message_stop.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_after_message_delta.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/eof_mid_content_block.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/error_after_message_delta.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/parallel_tool_use.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/stream_error.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/text_only.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/thinking_then_text.txt",
    "crates/paigasus-helikon-providers-anthropic/tests/fixtures/tool_use_then_continuation.txt",
    "crates/paigasus-helikon-providers-gemini/tests/fixtures/parallel_signed.sse",
    "crates/paigasus-helikon-providers-openai/tests/fixtures/chat_text_usage_trailing.txt",
];

/// Repo root, derived from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root must resolve from CARGO_MANIFEST_DIR")
}

/// Collect `.txt` and `.sse` files under `dir`. Symlinks are skipped.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("symlink_metadata {path:?}: {e}"));
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "txt" || e == "sse")
        {
            out.push(path);
        }
    }
}

/// Find the fixture files. The walk covers only `crates/*/tests/fixtures/`.
/// It does not cover the repo root, because `.claude/worktrees/` holds other checkouts.
fn fixtures(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let crates = root.join("crates");
    let entries = std::fs::read_dir(&crates).unwrap_or_else(|e| panic!("read_dir {crates:?}: {e}"));
    for entry in entries {
        let dir = entry
            .expect("dir entry")
            .path()
            .join("tests")
            .join("fixtures");
        if dir.is_dir() {
            collect(&dir, &mut out);
        }
    }
    out.sort();
    out
}

#[test]
fn every_fixture_has_a_provenance_header() {
    let root = repo_root();
    let files = fixtures(&root);
    assert!(files.len() >= 20, "found only {} fixtures", files.len());

    let mut failures = Vec::new();
    for entry in ALLOWLIST {
        if !root.join(entry).is_file() {
            failures.push(format!(
                "{entry}: the allowlist entry does not exist; remove it"
            ));
        }
    }
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let result = check(&text);
        let allowed = ALLOWLIST.contains(&rel.as_str());
        match (allowed, result) {
            (false, Err(e)) => failures.push(format!("{rel}: bad provenance header: {e:?}")),
            (true, Ok(())) => failures.push(format!(
                "{rel}: the header passes now; remove this file from ALLOWLIST"
            )),
            _ => {}
        }
    }
    assert!(failures.is_empty(), "{}\n", failures.join("\n"));
}

#[test]
fn cut_fixture_keeps_its_missing_final_newline() {
    let path = repo_root().join("crates/paigasus-helikon-providers-anthropic/tests/fixtures/body_cut_inside_message_stop.txt");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    assert_ne!(
        bytes.last(),
        Some(&b'\n'),
        "the cut fixture must end without a newline: it models a body cut inside message_stop (SMA-531 design)"
    );
}

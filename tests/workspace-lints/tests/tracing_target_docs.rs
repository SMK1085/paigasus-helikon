//! The `paigasus::<component>` set in source must equal the set documented in
//! the mdBook (SMA-557 D3).
//!
//! Components only. The `::<subsystem>` leaf is explicitly free to change, so
//! guarding it would redden CI on legitimate refactors.
//!
//! This asserts **presence, not guarantee**: the table's `Status` column
//! (`stable` / `provisional`) is ignored here. A provisional row is tracked
//! exactly like a stable one — the column is documentation for humans.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use paigasus_helikon_workspace_lints::scan_targets;

const BOOK_PAGE: &str = "docs/book/src/concepts/observability-evaluation.md";
const MARK_START: &str = "tracing-components:start";
const MARK_END: &str = "tracing-components:end";

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
///
/// Uses [`std::fs::symlink_metadata`] rather than [`Path::is_dir`]: `is_dir`
/// follows symlinks, so a symlink pointing at an ancestor directory would
/// recurse this walk unboundedly.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        let meta = std::fs::symlink_metadata(&path)
            .unwrap_or_else(|e| panic!("symlink_metadata {path:?}: {e}"));
        if meta.is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if meta.is_dir() {
            if name == "target" || name == ".git" {
                continue;
            }
            collect_rs(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// Components named in the marked region of the book page.
///
/// Parse rule, pinned deliberately: the **first cell of each table body row**
/// between the markers must be exactly `` `paigasus::<component>` ``. Header and
/// separator rows are skipped; any other non-empty row is a hard failure, so a
/// stray placeholder becomes a loud error instead of a phantom component.
fn documented_components(page: &str) -> BTreeSet<String> {
    let start = page
        .find(MARK_START)
        .unwrap_or_else(|| panic!("missing `{MARK_START}` marker in {BOOK_PAGE}"));
    let end = page
        .find(MARK_END)
        .unwrap_or_else(|| panic!("missing `{MARK_END}` marker in {BOOK_PAGE}"));
    assert!(
        end > start,
        "`{MARK_END}` precedes `{MARK_START}` in {BOOK_PAGE}"
    );

    let mut out = BTreeSet::new();
    for line in page[start..end].lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let first = line
            .trim_start_matches('|')
            .split('|')
            .next()
            .unwrap_or_default()
            .trim();
        // Header row and the `| --- |` separator carry no component. The
        // emptiness guard matters: `str::chars().all(...)` is vacuously true
        // on an empty string, so without it a row with an accidentally blank
        // first cell (`| | some-crate | ... | ... |`) would be misclassified
        // as a separator and silently skipped instead of hitting the hard
        // failure below, as the doc comment above promises.
        if first == "Component"
            || (!first.is_empty() && first.chars().all(|c| c == '-' || c == ':'))
        {
            continue;
        }
        let component = first
            .strip_prefix("`paigasus::")
            .and_then(|r| r.strip_suffix('`'))
            .unwrap_or_else(|| {
                panic!(
                    "row in the marked region of {BOOK_PAGE} has first cell {first:?}; \
                     expected exactly `paigasus::<component>`"
                )
            });
        assert!(
            !component.is_empty()
                && component
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "component {component:?} in {BOOK_PAGE} is not [a-z0-9_]+"
        );
        out.insert(component.to_owned());
    }
    out
}

#[test]
fn documented_components_match_source() {
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

    // Anti-vacuity: a truncated walk must fail, not pass. Set well below the
    // repo's actual file count so it does not couple to workspace size.
    assert!(
        files.len() >= 100,
        "scanned only {} .rs files — the walk is not reaching the workspace",
        files.len()
    );

    // Anti-vacuity: prove the scanner reads real source rather than returning a
    // constant. A path-existence assertion would not — `tests/` contributes no
    // components at all, so reaching it proves nothing about extraction.
    let probe = root.join("crates/paigasus-helikon-providers-openai/src/backend/chat.rs");
    let probe_src =
        std::fs::read_to_string(&probe).unwrap_or_else(|e| panic!("read {}: {e}", probe.display()));
    assert_eq!(
        scan_targets(&probe_src),
        BTreeSet::from(["openai".to_owned()]),
        "scan_targets did not extract `openai` from {}",
        probe.display()
    );

    let mut in_source = BTreeSet::new();
    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        in_source.extend(scan_targets(&src));
    }
    assert!(
        !in_source.is_empty(),
        "no `paigasus::` targets found in {} files — the scan is not working",
        files.len()
    );

    let page_path = root.join(BOOK_PAGE);
    let page = std::fs::read_to_string(&page_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", page_path.display()));
    let in_docs = documented_components(&page);
    assert!(
        !in_docs.is_empty(),
        "the marked region in {BOOK_PAGE} documents no components"
    );

    let undocumented: Vec<&String> = in_source.difference(&in_docs).collect();
    let stale: Vec<&String> = in_docs.difference(&in_source).collect();
    assert!(
        undocumented.is_empty() && stale.is_empty(),
        "tracing component drift between source and {BOOK_PAGE}:\n  \
         in source but not documented: {undocumented:?}\n  \
         documented but not in source: {stale:?}\n\
         Add or remove the row in the marked region. Renaming a component is a \
         breaking change (SMA-557 D1) — use a `BREAKING CHANGE:` footer."
    );
}

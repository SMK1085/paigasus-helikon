//! Every `tracing` macro invocation under `crates/*/src` must carry an
//! explicit, well-shaped, correctly-attributed `target: "paigasus::…"`
//! literal, and no `#[tracing::instrument]` attribute may appear there
//! (SMA-568).
//!
//! Five properties are enforced per invocation:
//!
//! 1. A `target:` argument is present at all ([`TargetArg::Absent`] fails).
//! 2. It is a string literal, not a computed expression
//!    ([`TargetArg::NonLiteral`] fails) — a computed target satisfies
//!    coverage but cannot be shape-checked, which is exactly how an event
//!    could be routed out of the namespace invisibly.
//! 3. The literal is shaped `paigasus::<component>::<subsystem>`, both
//!    segments `[a-z0-9_]+` ([`is_well_shaped`]).
//! 4. The literal's `<component>` matches the component registered for the
//!    crate the file lives in ([`CRATE_COMPONENTS`]).
//! 5. No `#[tracing::instrument]`/`#[instrument]` attribute appears anywhere
//!    in the file — the scanner keys on `ident !` and cannot see an
//!    attribute, so an instrumented function would silently emit its span on
//!    its module path instead of a `paigasus::` target (SMA-568 D7).
//!
//! Assertion 3 deliberately does not check the component against the
//! mdBook — `tracing_target_docs.rs` already does that, and duplicating it
//! here would mean two tests reddening for one cause with two messages.

use std::path::{Path, PathBuf};

use paigasus_helikon_workspace_lints::{
    allow_marker_lines, instrument_attribute_lines, scan_invocations, TargetArg,
    ALLOW_MARKER_COVERAGE,
};

/// Crate directory name (under `crates/`) to its registered
/// `paigasus::<component>`, for all 21 workspace members.
///
/// Ten of the twenty-one are reserved: they are name-claim stubs that emit no
/// tracing today, and per the design they must **not** appear in the
/// mdBook's component table (`tracing_target_docs.rs` asserts book and
/// source agree, and a row for an absent component fails it). They are still
/// listed here so a file that later appears under one of them resolves
/// instead of silently falling through the lookup, and so the
/// directory-existence assertion below covers all 21, not just the eleven
/// that are live.
///
/// Derivation rule for a future crate: strip `paigasus-helikon-`, then strip
/// a leading `providers-`, then replace `-` with `_`. The providers' bare
/// form (`openai`, not `providers_openai`) is a historical exception,
/// preserved because those names are the user-facing vendor names.
const CRATE_COMPONENTS: &[(&str, &str)] = &[
    ("paigasus-helikon", "facade"),
    ("paigasus-helikon-cli", "cli"),
    ("paigasus-helikon-core", "core"),
    ("paigasus-helikon-evals", "evals"),
    ("paigasus-helikon-macros", "macros"),
    ("paigasus-helikon-mcp", "mcp"),
    ("paigasus-helikon-providers-openai", "openai"),
    ("paigasus-helikon-providers-anthropic", "anthropic"),
    ("paigasus-helikon-providers-bedrock", "bedrock"),
    ("paigasus-helikon-providers-gemini", "gemini"),
    ("paigasus-helikon-providers-litellm", "litellm"),
    ("paigasus-helikon-runtime-tokio", "runtime_tokio"),
    ("paigasus-helikon-runtime-axum", "runtime_axum"),
    ("paigasus-helikon-runtime-actix", "runtime_actix"),
    ("paigasus-helikon-runtime-agentcore", "runtime_agentcore"),
    ("paigasus-helikon-runtime-temporal", "runtime_temporal"),
    ("paigasus-helikon-sessions-sqlite", "sessions_sqlite"),
    ("paigasus-helikon-sessions-postgres", "sessions_postgres"),
    ("paigasus-helikon-sessions-redis", "sessions_redis"),
    ("paigasus-helikon-sessions-testkit", "sessions_testkit"),
    ("paigasus-helikon-tools", "tools"),
];

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

/// Whether `target` is shaped exactly `paigasus::<component>::<subsystem>`,
/// with both segments non-empty and drawn from `[a-z0-9_]+`.
///
/// Hand-rolled rather than a regex: this crate takes no dependencies. A
/// naive "split on `::` and take the first two segments" implementation
/// would wrongly accept `paigasus::core::agent::extra` (four segments); this
/// one rejects it, because after stripping the `paigasus::` prefix it splits
/// **exactly once** and requires the remainder to hold no further `::`.
fn is_well_shaped(target: &str) -> bool {
    let Some(rest) = target.strip_prefix("paigasus::") else {
        return false;
    };
    let Some((component, subsystem)) = rest.split_once("::") else {
        return false;
    };
    if subsystem.contains("::") {
        return false;
    }
    is_valid_segment(component) && is_valid_segment(subsystem)
}

/// Whether `s` is a non-empty run of `[a-z0-9_]` bytes.
fn is_valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `.rs` files under `crates_dir` whose path contains a `src` path
/// component — this excludes e.g.
/// `crates/paigasus-helikon-providers-anthropic/tests/live.rs`, the one
/// non-`src` `tracing` user under `crates/`.
fn collect_src_rs(crates_dir: &Path) -> Vec<PathBuf> {
    let mut all = Vec::new();
    collect_rs(crates_dir, &mut all);
    all.into_iter()
        .filter(|p| p.components().any(|c| c.as_os_str() == "src"))
        .collect()
}

/// The crate directory name (e.g. `paigasus-helikon-core`) that `file`,
/// somewhere under `<crates_dir>/<crate-dir>/...`, belongs to.
fn crate_dir_of(file: &Path, crates_dir: &Path) -> String {
    let rel = file.strip_prefix(crates_dir).unwrap_or_else(|e| {
        panic!(
            "{} is not under {}: {e}",
            file.display(),
            crates_dir.display()
        )
    });
    rel.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .unwrap_or_else(|| panic!("{} has no leading path component", file.display()))
        .to_owned()
}

#[test]
fn shape_predicate_accepts_only_three_lowercase_segments() {
    assert!(is_well_shaped("paigasus::core::agent"));
    assert!(is_well_shaped("paigasus::runtime_agentcore::a2a"));

    assert!(!is_well_shaped("paigasus::core"), "two segments");
    assert!(
        !is_well_shaped("paigasus::core::agent::extra"),
        "four segments"
    );
    assert!(
        !is_well_shaped("paigasus::Core::agent"),
        "uppercase component"
    );
    assert!(
        !is_well_shaped("paigasus::core::Agent"),
        "uppercase subsystem"
    );
    assert!(!is_well_shaped("paigasus::::agent"), "empty component");
    assert!(!is_well_shaped("paigasus::core::"), "empty subsystem");
    assert!(
        !is_well_shaped("paigasus_helikon_core::agent"),
        "module path"
    );
    assert!(!is_well_shaped("hyper::client::pool"), "foreign namespace");
}

#[test]
fn every_tracing_site_under_crates_carries_a_well_shaped_paigasus_target() {
    let root = repo_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "resolved repo root {root:?} has no Cargo.toml"
    );

    let crates_dir = root.join("crates");
    assert!(
        crates_dir.is_dir(),
        "expected workspace directory {crates_dir:?}"
    );

    // Every registered crate must exist as a directory, so a renamed or
    // removed crate fails loudly instead of falling through the lookup.
    for (crate_dir, component) in CRATE_COMPONENTS {
        let dir = crates_dir.join(crate_dir);
        assert!(
            dir.is_dir(),
            "CRATE_COMPONENTS registers {crate_dir:?} -> {component:?}, \
             but {dir:?} does not exist"
        );
    }

    let files = collect_src_rs(&crates_dir);

    // Anti-vacuity: a truncated walk must fail, not pass. The real
    // population is 219, so this is a tripwire with headroom, not a
    // coupling to workspace size.
    assert!(
        files.len() >= 100,
        "scanned only {} .rs files under {crates_dir:?} `src` trees — \
         the walk is not reaching the workspace",
        files.len()
    );

    // Anti-vacuity: prove the scanner reads real source rather than
    // returning a constant.
    let probe = crates_dir.join("paigasus-helikon-providers-openai/src/backend/chat.rs");
    let probe_src =
        std::fs::read_to_string(&probe).unwrap_or_else(|e| panic!("read {}: {e}", probe.display()));
    let probe_invocations =
        scan_invocations(&probe_src).unwrap_or_else(|e| panic!("{}: {e}", probe.display()));
    assert!(
        probe_invocations
            .iter()
            .any(|inv| inv.target == TargetArg::Literal("paigasus::openai::chat".to_owned())),
        "scan_invocations did not extract `paigasus::openai::chat` from {}",
        probe.display()
    );

    let mut coverage_failures = Vec::new();
    let mut shape_failures = Vec::new();
    let mut component_failures = Vec::new();
    let mut instrument_failures = Vec::new();

    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));

        let crate_dir = crate_dir_of(file, &crates_dir);
        let component = CRATE_COMPONENTS
            .iter()
            .find(|(dir, _)| *dir == crate_dir)
            .map(|(_, component)| *component)
            .unwrap_or_else(|| {
                panic!(
                    "{} resolves to crate directory {crate_dir:?}, which is not in \
                     CRATE_COMPONENTS — add it",
                    file.display()
                )
            });

        // Anti-vacuity: the escape hatch is unused today, so a first use is
        // a fact a reviewer should see rather than a silent exemption.
        let allow_lines = allow_marker_lines(&src, ALLOW_MARKER_COVERAGE);
        assert!(
            allow_lines.is_empty(),
            "{}: uses the `{ALLOW_MARKER_COVERAGE}` escape hatch on line(s) {allow_lines:?} — \
             no site uses it today; this is the first use and needs a reviewer's eyes, \
             not a silently widened guard",
            file.display()
        );

        let invocations =
            scan_invocations(&src).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for inv in &invocations {
            // The marker suppresses the whole invocation: either trailing
            // its own line, or on the line immediately before it.
            //
            // Dead-by-design against this workspace's real source: the
            // anti-vacuity `assert!(allow_lines.is_empty(), ...)` above fires
            // first for every file scanned here, since no site under
            // `crates/*/src` uses the escape hatch today, so `suppressed` can
            // never actually be `true` in this test run. That composition —
            // an untargeted site carrying the marker actually being skipped —
            // is exercised separately, at unit level over an inline fixture,
            // by `escape_hatch_marker_suppresses_an_untargeted_site` below.
            // Don't "simplify" this check away just because it looks
            // unreachable; it is reachable the moment the anti-vacuity assert
            // above is ever relaxed or a marker is legitimately added.
            let suppressed = allow_lines.contains(&inv.line)
                || (inv.line > 1 && allow_lines.contains(&(inv.line - 1)));
            if suppressed {
                continue;
            }
            match &inv.target {
                TargetArg::Absent => {
                    coverage_failures.push(format!(
                        "{}:{}: `{}!` carries no `target:`",
                        file.display(),
                        inv.line,
                        inv.macro_name
                    ));
                }
                TargetArg::NonLiteral => {
                    coverage_failures.push(format!(
                        "{}:{}: `target:` is not a string literal; a computed target cannot \
                         be shape-checked",
                        file.display(),
                        inv.line
                    ));
                }
                TargetArg::Literal(t) => {
                    if !is_well_shaped(t) {
                        shape_failures.push(format!(
                            "{}:{}: target {t:?} is not shaped `paigasus::<component>::<subsystem>`",
                            file.display(),
                            inv.line
                        ));
                        continue;
                    }
                    // Already validated by `is_well_shaped` above, so both
                    // `strip_prefix` and `split_once` are guaranteed `Some`.
                    let target_component = t
                        .strip_prefix("paigasus::")
                        .and_then(|rest| rest.split_once("::"))
                        .map(|(component, _)| component)
                        .expect("is_well_shaped guarantees this shape");
                    if target_component != component {
                        component_failures.push(format!(
                            "{}:{}: target component {target_component:?} does not match crate \
                             {crate_dir:?}'s registered component {component:?}",
                            file.display(),
                            inv.line
                        ));
                    }
                }
            }
        }

        for line in instrument_attribute_lines(&src) {
            instrument_failures.push(format!(
                "{}:{line}: `#[tracing::instrument]` is not permitted under `crates/*/src`: \
                 the scanner keys on `ident !` and cannot see an attribute, so an instrumented \
                 function would silently emit on its module path (SMA-568 D7)",
                file.display()
            ));
        }
    }

    assert!(
        coverage_failures.is_empty(),
        "tracing target coverage violation(s):\n{}",
        coverage_failures.join("\n")
    );
    assert!(
        shape_failures.is_empty(),
        "tracing target shape violation(s):\n{}",
        shape_failures.join("\n")
    );
    assert!(
        component_failures.is_empty(),
        "tracing target component violation(s):\n{}",
        component_failures.join("\n")
    );
    assert!(
        instrument_failures.is_empty(),
        "`#[tracing::instrument]` violation(s):\n{}",
        instrument_failures.join("\n")
    );
}

/// Composition test for the escape hatch: an untargeted `tracing::warn!`
/// carrying the `allow(tracing-target-coverage)` marker must be suppressed
/// in both marker positions (preceding line, and trailing the invocation's
/// own line), while an identical site without the marker is reported.
///
/// This exercises the same `suppressed` composition used in
/// `every_tracing_site_under_crates_carries_a_well_shaped_paigasus_target`
/// above, over an inline fixture rather than by adding a marker to real
/// source — the real-source test's own anti-vacuity assert would otherwise
/// make that branch unreachable (spec §4.5).
#[test]
fn escape_hatch_marker_suppresses_an_untargeted_site() {
    fn reported_lines(src: &str) -> Vec<usize> {
        let allow_lines = allow_marker_lines(src, ALLOW_MARKER_COVERAGE);
        let invocations = scan_invocations(src).expect("well-formed fixture source");
        invocations
            .iter()
            .filter(|inv| {
                let suppressed = allow_lines.contains(&inv.line)
                    || (inv.line > 1 && allow_lines.contains(&(inv.line - 1)));
                !suppressed
            })
            .map(|inv| inv.line)
            .collect()
    }

    // Marker on the line immediately before the invocation: suppressed.
    let preceding = "// allow(tracing-target-coverage)\ntracing::warn!(\"m\");\n";
    assert_eq!(
        reported_lines(preceding),
        Vec::<usize>::new(),
        "a marker on the preceding line must suppress the untargeted site"
    );

    // Marker trailing the invocation's own line: suppressed.
    let trailing = "tracing::warn!(\"m\"); // allow(tracing-target-coverage)\n";
    assert_eq!(
        reported_lines(trailing),
        Vec::<usize>::new(),
        "a marker trailing the invocation's own line must suppress it"
    );

    // No marker at all: the identical site IS reported.
    let unmarked = "tracing::warn!(\"m\");\n";
    assert_eq!(
        reported_lines(unmarked),
        vec![1],
        "without the marker, the untargeted site must still be reported"
    );
}

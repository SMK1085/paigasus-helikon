//! UI tests for the LlmAgent typestate builder. The workflow restricts
//! execution to the latest-stable CI matrix row (via the existing
//! `--skip trybuild_ui` filter in `.github/workflows/ci.yml`) because
//! trybuild `.stderr` snapshots pin rustc diagnostic text byte-for-byte
//! and that text drifts across rustc releases.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/builder_missing_*.rs");
    t.compile_fail("tests/ui/builder_*_twice.rs");
    t.compile_fail("tests/ui/builder_source_blocks_build.rs");
    t.pass("tests/ui/builder_happy_path.rs");
    t.pass("tests/ui/builder_output_type_typed.rs");
}

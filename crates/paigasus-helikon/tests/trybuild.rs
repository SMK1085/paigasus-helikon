//! UI tests that pin the macro's facade-path resolution.
//!
//! See SMA-385 spec §5.5 — this test lives here (and not in
//! `paigasus-helikon-macros`) because keeping the dev-dep cycle
//! macros→facade blocks `cargo publish -p paigasus-helikon-macros`.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/facade_only_consumer.rs");
}

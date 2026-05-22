//! UI tests for #[tool] and tools!. Gated to stable rustc because
//! trybuild `.stderr` captures pin rustc diagnostic text byte-for-byte
//! and that text drifts across rustc releases — including between
//! stable and the 1.75 MSRV CI matrix entry.

#[rustversion::attr(not(stable), ignore)]
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/bad_*.rs");
    t.compile_fail("tests/ui/no_description.rs");
    t.compile_fail("tests/ui/empty_description.rs");
    t.pass("tests/ui/facade_only_consumer.rs");
}

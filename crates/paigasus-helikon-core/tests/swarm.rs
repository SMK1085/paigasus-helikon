//! SwarmAgent integration tests (SMA-333).

#[path = "common/mod.rs"]
mod common;

use paigasus_helikon_core::AgentError;

#[test]
fn max_handoffs_error_displays_limit() {
    let err = AgentError::MaxHandoffsExceeded { limit: 3 };
    assert_eq!(err.to_string(), "max handoffs (3) exceeded");
}

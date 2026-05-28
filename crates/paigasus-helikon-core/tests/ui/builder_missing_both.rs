//! `.build()` on the initial state — `.build` is not reachable on
//! `LlmAgentBuilder<…, NoName, NoModel>`.

use paigasus_helikon_core::LlmAgent;

fn main() {
    let _ = LlmAgent::builder::<()>().build();
}

//! `.name("x").build()` without `.model(…)` first — `.build` is not
//! reachable on `LlmAgentBuilder<…, HasName, NoModel>`.

use paigasus_helikon_core::LlmAgent;

fn main() {
    let _ = LlmAgent::builder::<()>()
        .name("triage")
        .build();
}

//! `.name("a").name("b")` — the second `.name` is not in scope once
//! the typestate has transitioned to `HasName`.

use paigasus_helikon_core::LlmAgent;

fn main() {
    let _ = LlmAgent::builder::<()>()
        .name("first")
        .name("second");
}

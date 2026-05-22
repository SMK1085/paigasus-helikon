//! Compile-pass: this file mentions only `paigasus_helikon` (the
//! facade), never `paigasus_helikon_core` directly. It locks the
//! proc-macro-crate auto-resolution: when only the facade is in the
//! dep graph, the macro must emit paths rooted at
//! `::paigasus_helikon::core::…`.

use std::sync::Arc;

use paigasus_helikon::core::{Tool, ToolContext, ToolError};
use paigasus_helikon::{tool, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct MyCtx;

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    a: i64,
    b: i64,
}

#[derive(Serialize, JsonSchema)]
struct AddOut {
    sum: i64,
}

/// Adds two numbers.
#[tool]
async fn add(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

fn main() {
    let _r: Vec<Arc<dyn Tool<MyCtx>>> = tools![add];
}

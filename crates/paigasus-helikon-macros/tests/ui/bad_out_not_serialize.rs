use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

// Missing `Serialize`.
struct O {}

/// Description.
#[tool]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}

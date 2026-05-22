use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

// Missing `JsonSchema`.
#[derive(Deserialize)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Description.
#[tool]
async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}

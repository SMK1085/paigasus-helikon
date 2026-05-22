use paigasus_helikon_core::ToolError;
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// First arg isn't &ToolContext<C>.
#[tool]
async fn nope(_ctx: &C, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}

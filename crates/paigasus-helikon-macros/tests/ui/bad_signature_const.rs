use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Const.
#[tool]
const async fn nope(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {}

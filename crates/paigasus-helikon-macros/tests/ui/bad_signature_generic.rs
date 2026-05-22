#![allow(unused_imports)]
use paigasus_helikon_core::{ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct C;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Generic.
#[tool]
async fn nope<T>(_ctx: &ToolContext<C>, _args: A) -> Result<O, ToolError>
where
    T: Send,
{
    Ok(O {})
}

fn main() {}
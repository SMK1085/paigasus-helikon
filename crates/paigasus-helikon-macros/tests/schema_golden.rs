//! AC #1: a two-arg tool with doc comments produces a JSON Schema
//! matching the checked-in golden file.

use paigasus_helikon_core::{Tool, ToolContext, ToolError};
use paigasus_helikon_macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct MyCtx;

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    /// First addend.
    a: i64,
    /// Second addend.
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

#[test]
fn add_schema_matches_golden() {
    let serialized = serde_json::to_string_pretty(add.schema()).unwrap();
    insta::assert_snapshot!(serialized);
}

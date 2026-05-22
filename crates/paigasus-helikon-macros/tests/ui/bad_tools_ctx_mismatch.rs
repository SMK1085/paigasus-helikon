//! Locks the canonical rustc diagnostic that surfaces when `tools!`
//! is given tools whose `Ctx` types don't match. The error text is
//! intentionally rustc-driven (`tools!` deliberately does NOT emit a
//! custom diagnostic — see SMA-315 spec §10) and is reproduced in the
//! `tools!` rustdoc as a grep target for users.

use std::sync::Arc;

use paigasus_helikon_core::{Tool, ToolContext, ToolError};
use paigasus_helikon_macros::{tool, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct CtxA;
struct CtxB;

#[derive(Deserialize, JsonSchema)]
struct A {}

#[derive(Serialize, JsonSchema)]
struct O {}

/// Bound to CtxA.
#[tool]
async fn for_a(_ctx: &ToolContext<CtxA>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

/// Bound to CtxB.
#[tool]
async fn for_b(_ctx: &ToolContext<CtxB>, _args: A) -> Result<O, ToolError> {
    Ok(O {})
}

fn main() {
    // Both tools share a tools![…] invocation but impl Tool<Ctx> for
    // different Ctx types; rustc must reject with a trait-bound error
    // naming the second tool.
    let _r: Vec<Arc<dyn Tool<CtxA>>> = tools![for_a, for_b];
}

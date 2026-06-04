//! End-to-end behavioral lock for #[tool] and tools!. Verifies the
//! contract specified in SMA-315's spec §6.3.
//!
//! The file-level `deny(non_snake_case)` makes step 10's
//! attribute-forwarding assertion load-bearing: if the macro fails
//! to forward `#[allow(non_snake_case)]` to the helper fn, the deny
//! turns the lint into a hard compile error.

#![deny(non_snake_case)]

use std::sync::Arc;

use anyhow::anyhow;
use paigasus_helikon_core::{
    CancellationToken, Tool, ToolContext, ToolError, ToolOutput, TracerHandle,
};
use paigasus_helikon_macros::{tool, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

struct MyCtx;

fn make_ctx() -> ToolContext<MyCtx> {
    ToolContext::new(
        Arc::new(MyCtx),
        TracerHandle::default(),
        CancellationToken::new(),
        0,
        paigasus_helikon_core::RunConfig::default().max_agent_depth,
    )
}

// ---------- Tool 1: AddArgs/AddOut both derive JsonSchema -------------------

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
///
/// Subsequent paragraph — must NOT appear in description().
#[tool]
async fn add(_ctx: &ToolContext<MyCtx>, args: AddArgs) -> Result<AddOut, ToolError> {
    Ok(AddOut {
        sum: args.a + args.b,
    })
}

// ---------- Tool 2: explicit description overrides doc comment --------------

/// Long rustdoc-style description that will not be used as the
/// tool description because the attr below takes precedence.
///
/// Including a second paragraph for thoroughness.
#[tool(description = "Short.")]
async fn explicit_desc(_ctx: &ToolContext<MyCtx>, args: AddArgs) -> Result<AddOut, ToolError> {
    Ok(AddOut {
        sum: args.a + args.b,
    })
}

// ---------- Tool 3: Out without JsonSchema → output_schema() = None ---------

#[derive(Serialize)]
struct OpaqueOut(String);

/// A tool whose output type does not derive JsonSchema.
#[tool]
async fn opaque(_ctx: &ToolContext<MyCtx>, args: AddArgs) -> Result<OpaqueOut, ToolError> {
    Ok(OpaqueOut(format!(
        "{}+{}={}",
        args.a,
        args.b,
        args.a + args.b
    )))
}

// ---------- Tool 4: anyhow body — `?` does the From conversion --------------

#[derive(Deserialize, JsonSchema)]
struct EmptyArgs {}

#[derive(Serialize, JsonSchema)]
struct EmptyOut {}

/// Always fails with an anyhow error.
#[tool]
async fn anyhow_failer(
    _ctx: &ToolContext<MyCtx>,
    _args: EmptyArgs,
) -> Result<EmptyOut, anyhow::Error> {
    Err(anyhow!("boom"))
}

// ---------- Tool 5: forwarded #[allow] + camelCase name --------------------

/// Legacy adder kept around for compatibility.
#[tool]
#[allow(non_snake_case)]
async fn legacyAdd(_ctx: &ToolContext<MyCtx>, args: AddArgs) -> Result<AddOut, ToolError> {
    Ok(AddOut {
        sum: args.a + args.b,
    })
}

// ---------- Tests ----------------------------------------------------------

#[tokio::test]
async fn registry_basics() {
    let registry: Vec<Arc<dyn Tool<MyCtx>>> = tools![add];
    assert_eq!(registry.len(), 1);
    assert_eq!(registry[0].name(), "add");
    assert_eq!(registry[0].description(), "Adds two numbers.");
}

#[tokio::test]
async fn attr_description_wins_over_doc() {
    assert_eq!(explicit_desc.description(), "Short.");
}

#[tokio::test]
async fn invoke_valid_args() {
    let ctx = make_ctx();
    let out = add.invoke(&ctx, json!({ "a": 2, "b": 3 })).await.unwrap();
    assert_eq!(out.content, json!({ "sum": 5 }));
}

#[tokio::test]
async fn invoke_invalid_args() {
    let ctx = make_ctx();
    let err = add
        .invoke(&ctx, json!({ "a": "not-a-number", "b": 3 }))
        .await
        .unwrap_err();
    match err {
        ToolError::InvalidArgs { schema_errors } => {
            assert!(!schema_errors.is_empty(), "schema_errors must be non-empty");
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

#[tokio::test]
async fn schema_returns_cached_reference() {
    let s1: &serde_json::Value = add.schema();
    let s2: &serde_json::Value = add.schema();
    assert!(
        std::ptr::eq(s1, s2),
        "OnceLock must hand back the same &Value across calls"
    );
}

#[tokio::test]
async fn output_schema_present_when_jsonschema_derived() {
    assert!(add.output_schema().is_some());
}

#[tokio::test]
async fn output_schema_absent_for_non_jsonschema_out() {
    assert!(opaque.output_schema().is_none());
}

#[tokio::test]
async fn anyhow_error_surfaces_as_tool_error_other() {
    let ctx = make_ctx();
    let err = anyhow_failer.invoke(&ctx, json!({})).await.unwrap_err();
    match err {
        ToolError::Other(e) => {
            assert!(e.to_string().contains("boom"));
        }
        other => panic!("expected Other(anyhow::Error), got {other:?}"),
    }
}

#[tokio::test]
async fn forwarded_allow_silences_camelcase_lint() {
    // If `#[allow(non_snake_case)]` did not reach the helper fn (the
    // body uses `legacyAdd` as the helper-fn ident inside the const _
    // block generated by #[tool]), the file-level `#![deny(non_snake_case)]`
    // would have failed compilation. Reaching this assertion proves
    // forwarding works.
    let _ = ToolOutput::new(json!({}));
    assert_eq!(legacyAdd.name(), "legacyAdd");
}

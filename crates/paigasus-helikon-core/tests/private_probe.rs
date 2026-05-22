//! Locks the autoref-specialization behavior of OutputSchemaProbe.
//! If this test breaks, the #[tool] macro's output_schema() codegen
//! will silently regress.

#![allow(clippy::needless_borrow)]

use paigasus_helikon_core::__private::{OutputSchemaProbe, OutputSchemaProbeSpec as _};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Serialize, JsonSchema)]
struct HasSchema {
    x: i32,
}

struct NoSchema;

#[test]
fn jsonschema_type_picks_specialized_arm() {
    let v = (&&OutputSchemaProbe::<HasSchema>::NEW).schema();
    assert!(v.is_some(), "Out: JsonSchema must produce Some(schema)");
}

#[test]
fn non_jsonschema_type_picks_fallback_arm() {
    let v = (&&OutputSchemaProbe::<NoSchema>::NEW).schema();
    assert!(v.is_none(), "Out without JsonSchema must produce None");
}

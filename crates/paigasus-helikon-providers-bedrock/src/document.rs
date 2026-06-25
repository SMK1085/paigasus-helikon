//! Document-block helpers for the Bedrock Converse API.
//!
//! The [`aws_smithy_types::Document`] type is a recursive JSON-like value
//! enum used as the `input` field in Bedrock `ToolUseBlock`s. This module
//! provides a converter from [`serde_json::Value`].
//!
//! Note: `arbitrary_precision` is **not** enabled in this workspace. All
//! number conversion goes through the `as_u64`/`as_i64`/`as_f64` accessors.

use aws_smithy_types::{Document, Number};
use serde_json::Value;
use std::collections::HashMap;

/// Convert a [`serde_json::Value`] into an [`aws_smithy_types::Document`].
///
/// Numbers follow the precision available without `arbitrary_precision`:
/// - Unsigned integers use [`Number::PosInt`].
/// - Signed negative integers use [`Number::NegInt`].
/// - Floating-point numbers use [`Number::Float`].
// Used by translate/request.rs (Task 7) — allow dead_code until that task lands.
#[allow(dead_code)]
pub(crate) fn value_to_document(v: &Value) -> Document {
    match v {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::String(s) => Document::String(s.clone()),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Document::Number(Number::PosInt(u))
            } else if let Some(i) = n.as_i64() {
                Document::Number(Number::NegInt(i))
            } else {
                // Falls through to f64; as_f64() is always Some when
                // arbitrary_precision is disabled and the value is finite.
                Document::Number(Number::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        Value::Array(arr) => Document::Array(arr.iter().map(value_to_document).collect()),
        Value::Object(map) => {
            let mut out: HashMap<String, Document> = HashMap::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), value_to_document(v));
            }
            Document::Object(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::Document;
    use serde_json::json;

    #[test]
    fn null_bool_string() {
        assert!(matches!(value_to_document(&json!(null)), Document::Null));
        assert!(matches!(
            value_to_document(&json!(true)),
            Document::Bool(true)
        ));
        assert!(matches!(value_to_document(&json!("x")), Document::String(s) if s == "x"));
    }

    #[test]
    fn positive_negative_and_float_numbers() {
        use aws_smithy_types::Number;
        assert!(matches!(
            value_to_document(&json!(7u64)),
            Document::Number(Number::PosInt(7))
        ));
        assert!(matches!(
            value_to_document(&json!(-7i64)),
            Document::Number(Number::NegInt(-7))
        ));
        assert!(
            matches!(value_to_document(&json!(1.5f64)), Document::Number(Number::Float(f)) if (f-1.5).abs()<f64::EPSILON)
        );
    }

    #[test]
    fn u64_above_i64_max_stays_posint() {
        use aws_smithy_types::Number;
        let big = json!(u64::MAX);
        assert!(
            matches!(value_to_document(&big), Document::Number(Number::PosInt(n)) if n == u64::MAX)
        );
    }

    #[test]
    fn nested_object_and_empty_array() {
        let d = value_to_document(&json!({"a":[1], "b":{}}));
        let Document::Object(m) = d else { panic!() };
        assert!(matches!(m.get("a"), Some(Document::Array(a)) if a.len() == 1));
        assert!(matches!(m.get("b"), Some(Document::Object(o)) if o.is_empty()));
    }
}

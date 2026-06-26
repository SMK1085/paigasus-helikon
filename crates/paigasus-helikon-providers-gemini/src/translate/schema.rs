//! Rewrite JSON Schema into the OpenAPI-3.0 subset Gemini accepts.

use serde_json::{json, Map, Value};

const MAX_DEPTH: usize = 64;

/// Keywords Gemini's schema validator rejects outright.
const STRIP: &[&str] = &[
    "$schema",
    "$id",
    "$anchor",
    "$comment",
    "additionalProperties",
    "unevaluatedProperties",
    "patternProperties",
    "examples",
    "default",
];

/// Rewrite `schema` into Gemini's OpenAPI-3.0 subset.
///
/// Inlines `$ref`, strips unsupported keywords, drops `format` values Gemini
/// doesn't recognize for their sibling `type` (see [`format_is_supported`]),
/// and preserves combinator meaning (`oneOf`→`anyOf`, `[T,null]`→`nullable:true`,
/// `const`→`enum:[v]`).
pub(crate) fn sanitize_schema(schema: &Value) -> Value {
    let defs = collect_defs(schema);
    rewrite(schema, &defs, 0, &mut Vec::new())
}

fn collect_defs(root: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for key in ["$defs", "definitions"] {
        if let Some(Value::Object(m)) = root.get(key) {
            for (k, v) in m {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

fn rewrite(node: &Value, defs: &Map<String, Value>, depth: usize, seen: &mut Vec<String>) -> Value {
    if depth > MAX_DEPTH {
        return json!({ "type": "object" });
    }
    let Value::Object(obj) = node else {
        return node.clone();
    };

    // 1. $ref inlining with cycle guard.
    if let Some(Value::String(r)) = obj.get("$ref") {
        let name = r.rsplit('/').next().unwrap_or_default().to_owned();
        if seen.contains(&name) {
            return json!({ "type": "object" });
        }
        if let Some(target) = defs.get(&name) {
            seen.push(name);
            let out = rewrite(target, defs, depth + 1, seen);
            seen.pop();
            return out;
        }
        return json!({ "type": "object" });
    }

    let mut out = Map::new();
    for (k, v) in obj {
        if STRIP.contains(&k.as_str()) || k == "$defs" || k == "definitions" {
            continue;
        }
        match k.as_str() {
            // 3. const -> enum:[v]
            "const" => {
                out.insert("enum".into(), json!([v.clone()]));
            }
            // type: [T, "null"] -> T + nullable:true;
            // type: [A, B, ...] (multiple non-null) -> anyOf to preserve every type.
            "type" if v.is_array() => {
                let arr = v.as_array().unwrap();
                let non_null: Vec<&Value> =
                    arr.iter().filter(|x| x.as_str() != Some("null")).collect();
                if arr.iter().any(|x| x.as_str() == Some("null")) {
                    out.insert("nullable".into(), Value::Bool(true));
                }
                if let [single] = non_null.as_slice() {
                    out.insert("type".into(), (*single).clone());
                } else if non_null.len() > 1 {
                    let members: Vec<Value> =
                        non_null.iter().map(|t| json!({ "type": t })).collect();
                    out.insert("anyOf".into(), Value::Array(members));
                }
                // no else: a type array with zero non-null entries emits no `type` key
            }
            // oneOf -> anyOf (recursing members)
            "oneOf" | "anyOf" => {
                let members: Vec<Value> = v
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|m| rewrite(m, defs, depth + 1, seen))
                            .collect()
                    })
                    .unwrap_or_default();
                // [T, {type:null}] -> nullable
                let nulls = members
                    .iter()
                    .any(|m| m.get("type").and_then(|t| t.as_str()) == Some("null"));
                let non_null: Vec<Value> = members
                    .into_iter()
                    .filter(|m| m.get("type").and_then(|t| t.as_str()) != Some("null"))
                    .collect();
                if nulls {
                    out.insert("nullable".into(), Value::Bool(true));
                }
                if non_null.len() == 1 {
                    if let Value::Object(only) = &non_null[0] {
                        for (kk, vv) in only {
                            out.entry(kk.clone()).or_insert_with(|| vv.clone());
                        }
                    }
                } else {
                    out.insert("anyOf".into(), Value::Array(non_null));
                }
            }
            "properties" => {
                let mut p = Map::new();
                if let Value::Object(props) = v {
                    for (pk, pv) in props {
                        p.insert(pk.clone(), rewrite(pv, defs, depth + 1, seen));
                    }
                }
                out.insert("properties".into(), Value::Object(p));
            }
            "items" => {
                out.insert("items".into(), rewrite(v, defs, depth + 1, seen));
            }
            // format: keep only values Gemini's OpenAPI-3.0 Schema subset
            // recognizes for the sibling `type`; drop everything else. A forced
            // `responseSchema` carrying an unrecognized format (e.g. `email`,
            // `uri`, `uuid`) is rejected by Gemini with a 400.
            "format" => {
                let ty = obj.get("type").and_then(Value::as_str);
                if v.as_str().is_some_and(|fmt| format_is_supported(ty, fmt)) {
                    out.insert("format".into(), v.clone());
                }
                // else: dropped (omitted from the rewritten object)
            }
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Whether `fmt` is a `format` value Gemini's OpenAPI-3.0 Schema subset
/// recognizes for the sibling `type` `ty`.
///
/// Any `format` outside this set is dropped during rewriting, because a forced
/// `responseSchema` carrying an unrecognized format (e.g. the JSON-Schema
/// string formats `email`, `uri`, `uuid`, `hostname`) is rejected by the Gemini
/// API with a 400.
///
/// Kept set (everything else dropped, including formats on any other / missing
/// `type`):
///
/// - `string`  → `enum`, `date-time`
/// - `integer` → `int32`, `int64`
/// - `number`  → `float`, `double`
fn format_is_supported(ty: Option<&str>, fmt: &str) -> bool {
    match ty {
        Some("string") => matches!(fmt, "enum" | "date-time"),
        Some("integer") => matches!(fmt, "int32" | "int64"),
        Some("number") => matches!(fmt, "float" | "double"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_unsupported_keywords() {
        let input = json!({
            "$schema": "http://json-schema.org/draft/2020-12/schema",
            "$id": "x", "additionalProperties": false,
            "type": "object",
            "properties": { "a": { "type": "string", "examples": ["x"] } }
        });
        let out = sanitize_schema(&input);
        assert!(out.get("$schema").is_none());
        assert!(out.get("$id").is_none());
        assert!(out.get("additionalProperties").is_none());
        assert!(out["properties"]["a"].get("examples").is_none());
        assert_eq!(out["properties"]["a"]["type"], "string");
    }

    #[test]
    fn inlines_ref_from_defs() {
        let input = json!({
            "type": "object",
            "properties": { "child": { "$ref": "#/$defs/Child" } },
            "$defs": { "Child": { "type": "object", "properties": { "n": { "type": "integer" } } } }
        });
        let out = sanitize_schema(&input);
        assert!(out.get("$defs").is_none());
        assert_eq!(out["properties"]["child"]["type"], "object");
        assert_eq!(
            out["properties"]["child"]["properties"]["n"]["type"],
            "integer"
        );
    }

    #[test]
    fn nullable_collapse_from_type_array() {
        let input = json!({ "type": ["string", "null"] });
        let out = sanitize_schema(&input);
        assert_eq!(out["type"], "string");
        assert_eq!(out["nullable"], true);
    }

    #[test]
    fn multi_type_array_becomes_anyof() {
        let input = json!({ "type": ["string", "integer"] });
        let out = sanitize_schema(&input);
        assert!(out.get("type").is_none());
        assert_eq!(
            out["anyOf"],
            json!([{ "type": "string" }, { "type": "integer" }])
        );
    }

    #[test]
    fn multi_type_array_with_null_becomes_anyof_and_nullable() {
        let input = json!({ "type": ["string", "integer", "null"] });
        let out = sanitize_schema(&input);
        assert_eq!(out["nullable"], true);
        assert_eq!(
            out["anyOf"],
            json!([{ "type": "string" }, { "type": "integer" }])
        );
    }

    #[test]
    fn oneof_becomes_anyof_and_const_becomes_enum() {
        let input = json!({
            "oneOf": [ { "type": "string" }, { "const": 5 } ]
        });
        let out = sanitize_schema(&input);
        assert!(out.get("oneOf").is_none());
        let any = out["anyOf"].as_array().unwrap();
        assert_eq!(any[0]["type"], "string");
        assert_eq!(any[1]["enum"], json!([5]));
    }

    #[test]
    fn drops_unsupported_string_format() {
        let input = json!({ "type": "string", "format": "email" });
        let out = sanitize_schema(&input);
        assert_eq!(out["type"], "string");
        assert!(out.get("format").is_none());
    }

    #[test]
    fn keeps_recognized_string_format() {
        let input = json!({ "type": "string", "format": "date-time" });
        let out = sanitize_schema(&input);
        assert_eq!(out["type"], "string");
        assert_eq!(out["format"], "date-time");
    }

    #[test]
    fn integer_format_kept_or_dropped_by_type() {
        let kept = sanitize_schema(&json!({ "type": "integer", "format": "int64" }));
        assert_eq!(kept["format"], "int64");

        let dropped = sanitize_schema(&json!({ "type": "integer", "format": "foo" }));
        assert!(dropped.get("format").is_none());
    }

    #[test]
    fn cycle_is_guarded() {
        // Self-referential $ref must not infinitely recurse.
        let input = json!({
            "type": "object",
            "properties": { "self": { "$ref": "#/$defs/Node" } },
            "$defs": { "Node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } } }
        });
        let out = sanitize_schema(&input);
        // Terminates; deep node degrades to an empty object at the depth/cycle guard.
        assert_eq!(out["properties"]["self"]["type"], "object");
    }
}

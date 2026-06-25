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
/// Inlines `$ref`, strips unsupported keywords, and preserves combinator
/// meaning (`oneOf`→`anyOf`, `[T,null]`→`nullable:true`, `const`→`enum:[v]`).
///
/// # Dead-code note
/// Consumed by `translate/tools.rs` + `response_format.rs` in later tasks.
#[allow(dead_code)]
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
            // type: [T, "null"] -> T + nullable:true
            "type" if v.is_array() => {
                let arr = v.as_array().unwrap();
                let non_null: Vec<&Value> =
                    arr.iter().filter(|x| x.as_str() != Some("null")).collect();
                if arr.iter().any(|x| x.as_str() == Some("null")) {
                    out.insert("nullable".into(), Value::Bool(true));
                }
                if let [single] = non_null.as_slice() {
                    out.insert("type".into(), (*single).clone());
                } else if let Some(first) = non_null.first() {
                    out.insert("type".into(), (*first).clone());
                }
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
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
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

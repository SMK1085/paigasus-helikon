//! Schema rewrite for Bedrock native structured output (`outputConfig.textFormat`).
//!
//! The policy is a denylist. The rewrite removes the keywords that Bedrock rejects
//! and keeps all other keywords. Bedrock does not enforce the removed limits.
//! The core function `validate_terminal` checks the result against the original
//! schema of the caller.
//!
//! The rewrite does not visit `contains`, `dependentSchemas`, the tuple form of
//! `items`, or the sub-schemas under `additionalProperties`, `patternProperties`,
//! `prefixItems`, `unevaluatedProperties` and `propertyNames`. The rewrite
//! passes these through, and Bedrock decides on them. The rewrite removes `not`.
//!
//! An object node under an `allOf` composition stays open (no added
//! `additionalProperties: false`), because the closed members would reject the
//! properties of the other members. The same rule applies to an object node
//! next to `oneOf` or `anyOf` (the schemars output for a flattened tagged
//! enum), also when it has no `properties`. The branches of such a node stay
//! open too. A
//! `$defs` entry that a `$ref` inside an `allOf` points to is still closed.

use serde_json::{Map, Value};

/// Keywords that the rewrite removes from every schema node.
const REMOVED_KEYWORDS: [&str; 11] = [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "maxItems",
    "uniqueItems",
    "not",
    "if",
    "then",
    "else",
];

/// Rewrite a raw JSON schema into the form that Bedrock native output accepts.
///
/// The function never fails and does not read the `Ruleset`.
pub(crate) fn native_output_schema(schema: &Value) -> Value {
    rewrite(schema, false)
}

fn rewrite(v: &Value, open: bool) -> Value {
    let Some(obj) = v.as_object() else {
        return v.clone();
    };
    let mut m: Map<String, Value> = obj.clone();

    // 1. Keyword removal.
    for key in REMOVED_KEYWORDS {
        m.remove(key);
    }
    if m.get("minItems")
        .and_then(Value::as_f64)
        .is_some_and(|n| n > 1.0)
    {
        m.remove("minItems");
    }

    // 2. `oneOf` becomes `anyOf`.
    if let Some(one) = m.remove("oneOf") {
        if m.contains_key("anyOf") {
            let entry = serde_json::json!({ "anyOf": one });
            match m.get_mut("allOf") {
                Some(Value::Array(all)) => all.push(entry),
                // A missing or malformed (non-array) `allOf` is replaced by the new one.
                _ => {
                    m.insert("allOf".to_owned(), Value::Array(vec![entry]));
                }
            }
        } else {
            m.insert("anyOf".to_owned(), one);
        }
    }

    // 3. Object nodes get `additionalProperties: false` unless they are a map shape.
    let is_object = match m.get("type") {
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(a)) => a.iter().any(|t| t == "object"),
        _ => false,
    } || m.contains_key("properties");
    let is_map = ["additionalProperties", "patternProperties", "propertyNames"]
        .iter()
        .any(|k| m.contains_key(*k));
    let has_all_of = m.contains_key("allOf");
    // An object node next to `anyOf` is a flattened composition. The parent and
    // the branches each hold a part of the properties. The parent can have no
    // `properties` of its own. A root `anyOf` without an object type (the
    // schemars `Option<Struct>`) is not flattened.
    let is_flattened = is_object && m.contains_key("anyOf");
    let child_open = open || has_all_of || is_flattened;
    if is_object && !is_map && !child_open {
        m.insert("additionalProperties".to_owned(), Value::Bool(false));
    }

    // 4. Recurse only into the subschema positions.
    for key in ["properties", "$defs", "definitions"] {
        if let Some(Value::Object(map)) = m.get_mut(key) {
            for child in map.values_mut() {
                *child = rewrite(child, false);
            }
        }
    }
    for key in ["anyOf", "allOf"] {
        if let Some(Value::Array(arr)) = m.get_mut(key) {
            for child in arr.iter_mut() {
                *child = rewrite(child, child_open);
            }
        }
    }
    if let Some(items @ Value::Object(_)) = m.get_mut("items") {
        *items = rewrite(items, false);
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kind() -> Value {
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Kind","description":"The kind of a transaction.","oneOf":[{"description":"Money in.","type":"string","const":"deposit"},{"description":"Money out.","type":"string","const":"withdrawal"}]})
    }
    fn option_kind() -> Value {
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Nullable_Kind","anyOf":[{"$ref":"#/$defs/Kind"},{"type":"null"}],"$defs":{"Kind":{"description":"The kind of a transaction.","oneOf":[{"description":"Money in.","type":"string","const":"deposit"},{"description":"Money out.","type":"string","const":"withdrawal"}]}}})
    }
    fn option_money() -> Value {
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Nullable_Money","anyOf":[{"$ref":"#/$defs/Money"},{"type":"null"}],"$defs":{"Money":{"type":"object","properties":{"amount":{"type":"number","format":"double"},"currency":{"type":"string"}},"required":["amount","currency"]}}})
    }
    fn transaction() -> Value {
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Transaction","type":"object","properties":{"count":{"type":"integer","format":"uint32","minimum":0},"kind":{"anyOf":[{"$ref":"#/$defs/Kind"},{"type":"null"}]},"money":{"anyOf":[{"$ref":"#/$defs/Money"},{"type":"null"}]},"tags":{"type":"object","additionalProperties":{"type":"string"}}},"required":["count","tags"],"$defs":{"Kind":{"description":"The kind of a transaction.","oneOf":[{"description":"Money in.","type":"string","const":"deposit"},{"description":"Money out.","type":"string","const":"withdrawal"}]},"Money":{"type":"object","properties":{"amount":{"type":"number","format":"double"},"currency":{"type":"string"}},"required":["amount","currency"]}}})
    }
    fn node() -> Value {
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema","title":"Node","type":"object","properties":{"children":{"type":"array","items":{"$ref":"#"}},"name":{"type":"string"}},"required":["name","children"]})
    }

    #[test]
    fn native_schema_adds_additional_properties_false_to_object_nodes() {
        assert_eq!(
            native_output_schema(&json!({"type":"object"})),
            json!({"type":"object","additionalProperties":false})
        );
        assert_eq!(
            native_output_schema(&json!({"type":["object","null"]})),
            json!({"type":["object","null"],"additionalProperties":false})
        );
        assert_eq!(
            native_output_schema(&json!({"properties":{"a":{"type":"string"}}})),
            json!({"properties":{"a":{"type":"string"}},"additionalProperties":false})
        );
        assert_eq!(
            native_output_schema(&json!({"type":"string"})),
            json!({"type":"string"})
        );
    }

    #[test]
    fn native_schema_properties_next_to_one_of_stay_open() {
        let out = native_output_schema(
            &json!({"type":"object","properties":{"amount":{"type":"number","format":"double"}},"required":["amount"],"oneOf":[{"type":"object","properties":{"kind":{"type":"string","const":"card"},"last4":{"type":"string"}},"required":["kind","last4"]},{"type":"object","properties":{"kind":{"type":"string","const":"cash"}},"required":["kind"]}]}),
        );
        assert!(out.get("oneOf").is_none());
        assert_eq!(out["required"], json!(["amount"]));
        assert!(out.get("additionalProperties").is_none());
        let branches = out["anyOf"].as_array().expect("anyOf array");
        assert_eq!(branches.len(), 2);
        for branch in branches {
            assert!(branch.get("additionalProperties").is_none());
            assert!(branch.get("required").is_some());
        }
    }

    #[test]
    fn native_schema_object_next_to_one_of_without_properties_stays_open() {
        let out = native_output_schema(
            &json!({"type":"object","oneOf":[{"type":"object","properties":{"kind":{"const":"a"}},"required":["kind"]},{"type":"object","properties":{"kind":{"const":"b"}},"required":["kind"]}]}),
        );
        assert!(out.get("oneOf").is_none());
        assert!(out.get("additionalProperties").is_none());
        let branches = out["anyOf"].as_array().expect("anyOf array");
        assert_eq!(branches.len(), 2);
        for branch in branches {
            assert!(branch.get("additionalProperties").is_none());
        }
    }

    #[test]
    fn native_schema_renames_one_of_to_any_of() {
        assert_eq!(
            native_output_schema(&json!({"oneOf":[{"type":"string"},{"type":"integer"}]})),
            json!({"anyOf":[{"type":"string"},{"type":"integer"}]})
        );
    }

    #[test]
    fn native_schema_removes_numeric_limits() {
        assert_eq!(
            native_output_schema(
                &json!({"type":"number","minimum":0,"maximum":9,"exclusiveMinimum":0,"exclusiveMaximum":9,"multipleOf":2})
            ),
            json!({"type":"number"})
        );
    }

    #[test]
    fn native_schema_removes_max_items_and_unique_items() {
        assert_eq!(
            native_output_schema(&json!({"type":"array","maxItems":3,"uniqueItems":true})),
            json!({"type":"array"})
        );
    }

    #[test]
    fn native_schema_removes_min_items_above_one_keeps_zero_and_one() {
        for (n, kept) in [(0, true), (1, true), (2, false)] {
            let out = native_output_schema(&json!({"type":"array","minItems":n}));
            let expected = if kept {
                json!({"type":"array","minItems":n})
            } else {
                json!({"type":"array"})
            };
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn native_schema_removes_not_if_then_else() {
        assert_eq!(
            native_output_schema(
                &json!({"type":"string","not":{"const":"x"},"if":{},"then":{},"else":{}})
            ),
            json!({"type":"string"})
        );
    }

    #[test]
    fn native_schema_keeps_other_keywords() {
        let s = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","title":"T","description":"d","type":"string","enum":["a","b"],"const":"a","format":"date","pattern":"^a","minLength":1,"maxLength":5,"default":"a","examples":["a"]});
        assert_eq!(native_output_schema(&s), s);
        assert_eq!(
            native_output_schema(&json!({"type":"object","required":["a"]})),
            json!({"type":"object","required":["a"],"additionalProperties":false})
        );
    }

    #[test]
    fn native_schema_recurses_into_subschemas() {
        let input = json!({
            "type":"object",
            "properties":{
                "a":{"type":"object","properties":{"n":{"type":"number","minimum":1}}},
                "b":{"type":"array","items":{"type":"object","properties":{"n":{"type":"integer","minimum":1}}}}
            },
            "$defs":{"D":{"type":"object","properties":{"n":{"type":"number","minimum":1}}}},
            "definitions":{"E":{"type":"object","properties":{"n":{"type":"number","minimum":1}}}},
            "anyOf":[{"type":"object","properties":{"n":{"type":"number","minimum":1}}}],
            "allOf":[{"type":"object","properties":{"n":{"type":"number","minimum":1}}}]
        });
        let nested = json!({"type":"object","properties":{"n":{"type":"number"}},"additionalProperties":false});
        let open = json!({"type":"object","properties":{"n":{"type":"number"}}});
        let expected = json!({
            "type":"object",
            "properties":{
                "a":nested.clone(),
                "b":{"type":"array","items":{"type":"object","properties":{"n":{"type":"integer"}},"additionalProperties":false}}
            },
            "$defs":{"D":nested.clone()},
            "definitions":{"E":nested.clone()},
            "anyOf":[open.clone()],
            "allOf":[open]
        });
        assert_eq!(native_output_schema(&input), expected);
    }

    #[test]
    fn native_schema_passes_through_unsafe_shapes() {
        for s in [
            json!({"type":"object","additionalProperties":true}),
            json!({"type":"object","additionalProperties":{"type":"string"}}),
            json!({"type":"object","patternProperties":{"^a":{"type":"string"}}}),
            json!({"type":"object","propertyNames":{"pattern":"^a"}}),
            json!({"type":"array","prefixItems":[{"type":"string"}]}),
            json!(true),
            json!({}),
            json!({"type":"array","items":{"$ref":"#"}}),
        ] {
            assert_eq!(native_output_schema(&s), s);
        }
    }

    #[test]
    fn native_schema_keeps_property_names_and_values_that_match_keywords() {
        let input = json!({"type":"object","properties":{"minimum":{"type":"number"},"not":{"type":"string"},"if":{"type":"boolean"}},"default":{"minimum":5,"not":1},"examples":[{"maximum":2}]});
        let mut expected = input.clone();
        expected["additionalProperties"] = json!(false);
        assert_eq!(native_output_schema(&input), expected);
    }

    #[test]
    fn native_schema_one_of_next_to_any_of_keeps_both() {
        assert_eq!(
            native_output_schema(
                &json!({"anyOf":[{"type":"string"}],"oneOf":[{"minLength":1},{"maxLength":9}]})
            ),
            json!({"anyOf":[{"type":"string"}],"allOf":[{"anyOf":[{"minLength":1},{"maxLength":9}]}]})
        );
    }

    #[test]
    fn native_schema_schemars_unit_enum_with_docs() {
        let mut expected = kind();
        let one = expected.as_object_mut().unwrap().remove("oneOf").unwrap();
        expected["anyOf"] = one;
        assert_eq!(native_output_schema(&kind()), expected);
    }

    #[test]
    fn native_schema_schemars_option_enum() {
        let mut expected = option_kind();
        let d = expected["$defs"]["Kind"].as_object_mut().unwrap();
        let one = d.remove("oneOf").unwrap();
        d.insert("anyOf".to_owned(), one);
        assert_eq!(native_output_schema(&option_kind()), expected);
    }

    #[test]
    fn native_schema_schemars_option_struct() {
        let mut expected = option_money();
        expected["$defs"]["Money"]["additionalProperties"] = json!(false);
        assert_eq!(native_output_schema(&option_money()), expected);
    }

    #[test]
    fn native_schema_schemars_u32_fields() {
        let out = native_output_schema(&transaction());
        assert_eq!(
            out["properties"]["count"],
            json!({"type":"integer","format":"uint32"})
        );
        assert_eq!(out["additionalProperties"], json!(false));
        assert_eq!(out["$defs"]["Money"]["additionalProperties"], json!(false));
    }

    #[test]
    fn native_schema_schemars_hashmap_field_passes_through() {
        let out = native_output_schema(&transaction());
        assert_eq!(
            out["properties"]["tags"],
            json!({"type":"object","additionalProperties":{"type":"string"}})
        );
    }

    #[test]
    fn native_schema_schemars_recursive_type_passes_through() {
        let out = native_output_schema(&node());
        assert_eq!(out["properties"]["children"]["items"], json!({"$ref":"#"}));
        assert_eq!(out["additionalProperties"], json!(false));
    }

    #[test]
    fn native_schema_all_of_members_stay_open() {
        let input = json!({"allOf":[{"type":"object","properties":{"a":{}}},{"type":"object","properties":{"b":{}}}]});
        assert_eq!(native_output_schema(&input), input);
    }

    #[test]
    fn native_schema_merged_one_of_branches_stay_open() {
        let input = json!({"anyOf":[{"type":"object","properties":{"a":{}}}],"oneOf":[{"type":"object","properties":{"b":{},"n":{"type":"number"}},"minimum":3}]});
        let out = native_output_schema(&input);
        assert_eq!(
            out,
            json!({"anyOf":[{"type":"object","properties":{"a":{}}}],"allOf":[{"anyOf":[{"type":"object","properties":{"b":{},"n":{"type":"number"}}}]}]})
        );
    }

    #[test]
    fn native_schema_nested_property_under_all_of_is_closed() {
        let input = json!({"allOf":[{"type":"object","properties":{"p":{"type":"object","properties":{"x":{"type":"string"}}}}}]});
        let out = native_output_schema(&input);
        assert!(out["allOf"][0].get("additionalProperties").is_none());
        assert_eq!(
            out["allOf"][0]["properties"]["p"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn native_schema_tuple_items_array_is_kept() {
        let s = json!({"type":"array","items":[{"type":"object","properties":{"a":{}}},{"type":"number","minimum":1}]});
        assert_eq!(native_output_schema(&s), s);
    }

    #[test]
    fn native_schema_properties_with_pattern_properties_not_closed() {
        let s = json!({"type":"object","properties":{"a":{"type":"string"}},"patternProperties":{"^b":{"type":"string"}}});
        assert_eq!(native_output_schema(&s), s);
    }
}

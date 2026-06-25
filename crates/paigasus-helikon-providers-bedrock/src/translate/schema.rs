//! JSON schema translation rulesets for the Bedrock Converse API.
//!
//! The primary entry point is [`rewrite_tool_schema`], which transforms a
//! JSON Schema value into a Bedrock-compatible form according to the given
//! [`Ruleset`].

/// Selects the rewrite strategy for tool schemas.
///
/// Currently the only variant is [`Ruleset::Strict`], which:
/// - inlines all `$ref`/`$defs`/`definitions` references,
/// - collapses `oneOf`/`anyOf`/`allOf` into a single relaxed object,
/// - strips keywords unsupported by the Bedrock Converse API.
///
/// The `#[non_exhaustive]` attribute allows future variants without a
/// breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ruleset {
    /// The Bedrock strict rewrite: inline refs, collapse combinators, strip
    /// unsupported keywords. Does **not** add `additionalProperties:false`
    /// or promote `required`.
    Strict,
}

impl Ruleset {
    /// Return the appropriate [`Ruleset`] for the given [`crate::ModelFamily`].
    ///
    /// All Bedrock model families currently use [`Ruleset::Strict`].
    pub fn for_family(_f: crate::ModelFamily) -> Self {
        Ruleset::Strict
    }
}

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum recursion depth before substituting a terminal `{"type":"object"}`.
const MAX_DEPTH: usize = 64;

/// Keywords stripped from every schema node before returning.
const UNSUPPORTED_KEYS: &[&str] = &[
    "$schema", "$id", "$anchor", "format", "examples", "default", "$comment",
];

// ── Public entry point ────────────────────────────────────────────────────────

/// Rewrite `schema` into a Bedrock-compatible tool schema using the given
/// [`Ruleset`].
///
/// The transform is **total** (never panics) and **idempotent**
/// (`rewrite(rewrite(x)) == rewrite(x)`).
///
/// # Transform summary (Strict ruleset)
/// 1. Inline all `$ref` references from `$defs`/`definitions`.
/// 2. Collapse `oneOf`/`anyOf`/`allOf` into one relaxed object.
/// 3. Strip unsupported keywords: `$schema`, `$id`, `$anchor`, `format`,
///    `examples`, `default`, `$comment`.
/// 4. Cycles or nodes exceeding the internal depth limit (64) become `{"type":"object"}`.
pub fn rewrite_tool_schema(schema: &serde_json::Value, ruleset: Ruleset) -> serde_json::Value {
    match ruleset {
        Ruleset::Strict => {
            // Extract the top-level $defs / definitions map (if any).
            let empty_map = serde_json::Map::new();
            let defs: &serde_json::Map<String, serde_json::Value> = schema
                .as_object()
                .and_then(|o| o.get("$defs").or_else(|| o.get("definitions")))
                .and_then(|v| v.as_object())
                .unwrap_or(&empty_map);

            let mut rewriter = Rewriter {
                defs,
                chain: Vec::new(),
                depth: 0,
            };
            let mut result = rewriter.rewrite_node(schema);

            // Remove $defs / definitions from the root output — they have been
            // fully inlined.
            if let Some(obj) = result.as_object_mut() {
                obj.remove("$defs");
                obj.remove("definitions");
            }
            result
        }
    }
}

// ── Rewriter ─────────────────────────────────────────────────────────────────

/// Stateful rewriter that carries the definition map and cycle-detection chain.
struct Rewriter<'a> {
    /// Top-level definitions map extracted from the root schema.
    defs: &'a serde_json::Map<String, serde_json::Value>,
    /// Stack of definition names currently being expanded (cycle detection).
    chain: Vec<String>,
    /// Current recursion depth (over-depth guard).
    depth: usize,
}

/// Canonical terminal node returned for cycles and over-depth situations.
#[inline]
fn terminal_object() -> serde_json::Value {
    serde_json::json!({"type": "object"})
}

impl<'a> Rewriter<'a> {
    /// Rewrite a single schema node recursively.
    fn rewrite_node(&mut self, node: &serde_json::Value) -> serde_json::Value {
        // Guard: over-depth
        if self.depth >= MAX_DEPTH {
            return terminal_object();
        }

        let Some(obj) = node.as_object() else {
            // Non-object values (arrays, strings, …) pass through unchanged.
            return node.clone();
        };

        // ── 1. $ref handling ─────────────────────────────────────────────────
        if let Some(ref_val) = obj.get("$ref") {
            return self.handle_ref(ref_val, obj);
        }

        // ── 2. Combinator handling ───────────────────────────────────────────
        for combinator in ["oneOf", "anyOf", "allOf"] {
            if let Some(variants_val) = obj.get(combinator) {
                if let Some(variants) = variants_val.as_array() {
                    return self.collapse_combinator(variants);
                }
            }
        }

        // ── 3. Plain object: strip, recurse into children ───────────────────
        let mut out = serde_json::Map::new();
        for (key, val) in obj {
            if UNSUPPORTED_KEYS.contains(&key.as_str()) {
                continue;
            }
            // Skip $defs / definitions — they are handled at the root level.
            if key == "$defs" || key == "definitions" {
                continue;
            }
            let new_val = match key.as_str() {
                "properties" => self.recurse_properties(val),
                "items" => self.recurse_items(val),
                "additionalProperties" => {
                    if val.is_object() {
                        self.depth += 1;
                        let v = self.rewrite_node(val);
                        self.depth -= 1;
                        v
                    } else {
                        val.clone()
                    }
                }
                _ => val.clone(),
            };
            out.insert(key.clone(), new_val);
        }
        serde_json::Value::Object(out)
    }

    /// Handle a `$ref` keyword.  Sibling keys (after stripping unsupported
    /// ones) are merged into the resolved schema (siblings win on collision).
    fn handle_ref(
        &mut self,
        ref_val: &serde_json::Value,
        siblings: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Value {
        let ref_str = match ref_val.as_str() {
            Some(s) => s,
            None => return terminal_object(),
        };

        // Resolve the reference target.
        let resolved_clone: serde_json::Value = match self.resolve_ref(ref_str) {
            Some(v) => v.clone(),
            None => return terminal_object(),
        };

        // Extract the definition name for cycle detection (only for local refs).
        let def_name = local_def_name(ref_str).map(|s| s.to_owned());

        if let Some(ref name) = def_name {
            if self.chain.contains(name) {
                // Cycle detected → terminal
                return terminal_object();
            }
            self.chain.push(name.clone());
        }

        self.depth += 1;
        let mut expanded = self.rewrite_node(&resolved_clone);
        self.depth -= 1;

        if let Some(ref name) = def_name {
            self.chain.retain(|n| n != name);
        }

        // Merge sibling keys (siblings win).  Strip unsupported siblings.
        if let Some(exp_obj) = expanded.as_object_mut() {
            for (key, val) in siblings {
                if key == "$ref" {
                    continue;
                }
                if UNSUPPORTED_KEYS.contains(&key.as_str()) {
                    continue;
                }
                exp_obj.insert(key.clone(), val.clone());
            }
        }

        expanded
    }

    /// Resolve a `$ref` string against the top-level definitions map.
    ///
    /// Supports `#/$defs/<name>` and `#/definitions/<name>`.
    /// Returns `None` for external / unresolvable refs.
    fn resolve_ref(&self, ref_str: &str) -> Option<&serde_json::Value> {
        let name = local_def_name(ref_str)?;
        self.defs.get(name)
    }

    /// Collapse a `oneOf`/`anyOf`/`allOf` combinator into one relaxed object.
    ///
    /// Algorithm:
    /// 1. Rewrite each variant (resolving any refs within).
    /// 2. Collect variant objects; non-object variants are skipped.
    /// 3. Detect a "shared tag key": a property present in **all** variants
    ///    whose schema is a `const` string or a single-value `enum` string.
    /// 4. Emit `{"type":"string","enum":[tags…]}` for the tag key.
    /// 5. Union the remaining properties (first-wins on collision).
    /// 6. Return `{"type":"object","properties":<union>}` (drop `required` and
    ///    `additionalProperties` — the Strict ruleset must not inject these).
    ///    If the union is empty, return `{"type":"object"}` with no `properties`.
    fn collapse_combinator(&mut self, variants: &[serde_json::Value]) -> serde_json::Value {
        // Rewrite each variant first.
        let rewritten: Vec<serde_json::Value> = variants
            .iter()
            .map(|v| {
                self.depth += 1;
                let r = self.rewrite_node(v);
                self.depth -= 1;
                r
            })
            .collect();

        // Only consider object variants.
        let obj_variants: Vec<&serde_json::Map<String, serde_json::Value>> =
            rewritten.iter().filter_map(|v| v.as_object()).collect();

        if obj_variants.is_empty() {
            return terminal_object();
        }

        // Collect properties maps from all variants.
        let props_maps: Vec<&serde_json::Map<String, serde_json::Value>> = obj_variants
            .iter()
            .filter_map(|v| v.get("properties").and_then(|p| p.as_object()))
            .collect();

        // Detect shared tag key.
        // A "tag key" is a property present in ALL variants whose schema is a
        // `const` string or single-value `enum` string.
        let tag_key: Option<(String, Vec<serde_json::Value>)> =
            if props_maps.len() == obj_variants.len() {
                // All variants have a `properties` map — look for a shared tag.
                find_shared_tag(&props_maps)
            } else {
                None
            };

        // Build the union of non-tag properties (first-wins on collision).
        let mut union_props: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

        if let Some((ref tag_name, ref tag_values)) = tag_key {
            // Insert the tag property as an enum of all tag strings.
            union_props.insert(
                tag_name.clone(),
                serde_json::json!({"type": "string", "enum": tag_values}),
            );
        }

        // Union all other properties from every variant.
        for props in &props_maps {
            for (key, val) in *props {
                if let Some((ref tag_name, _)) = tag_key {
                    if key == tag_name {
                        continue; // already handled
                    }
                }
                union_props
                    .entry(key.clone())
                    .or_insert_with(|| val.clone());
            }
        }

        if union_props.is_empty() {
            terminal_object()
        } else {
            serde_json::json!({"type": "object", "properties": union_props})
        }
    }

    /// Recurse into a `properties` object (the value of the `properties` key).
    fn recurse_properties(&mut self, val: &serde_json::Value) -> serde_json::Value {
        let Some(props) = val.as_object() else {
            return val.clone();
        };
        let mut out = serde_json::Map::new();
        for (key, prop_schema) in props {
            self.depth += 1;
            let rewritten = self.rewrite_node(prop_schema);
            self.depth -= 1;
            out.insert(key.clone(), rewritten);
        }
        serde_json::Value::Object(out)
    }

    /// Recurse into an `items` value (either an object schema or an array of
    /// schemas).
    fn recurse_items(&mut self, val: &serde_json::Value) -> serde_json::Value {
        if val.is_object() {
            self.depth += 1;
            let r = self.rewrite_node(val);
            self.depth -= 1;
            r
        } else if let Some(arr) = val.as_array() {
            let rewritten: Vec<serde_json::Value> = arr
                .iter()
                .map(|item| {
                    self.depth += 1;
                    let r = self.rewrite_node(item);
                    self.depth -= 1;
                    r
                })
                .collect();
            serde_json::Value::Array(rewritten)
        } else {
            val.clone()
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract the definition name from a local `$ref` string.
///
/// Handles `#/$defs/<name>` and `#/definitions/<name>`.
/// Returns `None` for anything else (external / unsupported).
fn local_def_name(ref_str: &str) -> Option<&str> {
    if let Some(name) = ref_str.strip_prefix("#/$defs/") {
        return Some(name);
    }
    if let Some(name) = ref_str.strip_prefix("#/definitions/") {
        return Some(name);
    }
    None
}

/// Detect a shared "tag" key across all variant property maps.
///
/// A property is a shared tag if:
/// - It is present in **every** variant's `properties` map.
/// - Its schema in every variant has either `"const": <string>` or
///   `"enum": [<single-string>]`.
///
/// Returns `Some((tag_key, [tag_values…]))` where `tag_values` preserves
/// variant order and contains no duplicates (first occurrence wins).
fn find_shared_tag(
    props_maps: &[&serde_json::Map<String, serde_json::Value>],
) -> Option<(String, Vec<serde_json::Value>)> {
    if props_maps.is_empty() {
        return None;
    }

    // Candidate keys: all keys in the first variant.
    let candidates: Vec<&str> = props_maps[0].keys().map(|k| k.as_str()).collect();

    for candidate in candidates {
        // Check if the candidate is a tag key in all variants.
        let mut tag_values: Vec<serde_json::Value> = Vec::new();
        let mut all_match = true;

        for props in props_maps {
            match props.get(candidate) {
                Some(schema) => {
                    if let Some(tag_val) = extract_const_string(schema) {
                        if !tag_values.contains(&tag_val) {
                            tag_values.push(tag_val);
                        }
                    } else {
                        all_match = false;
                        break;
                    }
                }
                None => {
                    all_match = false;
                    break;
                }
            }
        }

        if all_match && !tag_values.is_empty() {
            return Some((candidate.to_owned(), tag_values));
        }
    }

    None
}

/// Extract the string value from a `const` or single-element `enum` schema.
///
/// Returns `None` if the schema is not a string-valued const/enum.
fn extract_const_string(schema: &serde_json::Value) -> Option<serde_json::Value> {
    let obj = schema.as_object()?;

    // {"const": "SomeString"}
    if let Some(const_val) = obj.get("const") {
        if const_val.is_string() {
            return Some(const_val.clone());
        }
    }

    // {"enum": ["SomeString"]}
    if let Some(enum_val) = obj.get("enum") {
        if let Some(arr) = enum_val.as_array() {
            if arr.len() == 1 && arr[0].is_string() {
                return Some(arr[0].clone());
            }
        }
    }

    None
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strict(v: serde_json::Value) -> serde_json::Value {
        rewrite_tool_schema(&v, Ruleset::Strict)
    }

    fn has_key_anywhere(v: &serde_json::Value, key: &str) -> bool {
        match v {
            serde_json::Value::Object(m) => {
                m.contains_key(key) || m.values().any(|c| has_key_anywhere(c, key))
            }
            serde_json::Value::Array(a) => a.iter().any(|c| has_key_anywhere(c, key)),
            _ => false,
        }
    }

    #[test]
    fn inlines_defs_ref() {
        let input = json!({
            "type":"object",
            "properties": {"inner": {"$ref":"#/$defs/Inner"}},
            "$defs": {"Inner": {"type":"object","properties":{"x":{"type":"string"}}}}
        });
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "$ref"));
        assert!(!has_key_anywhere(&out, "$defs"));
        assert_eq!(
            out["properties"]["inner"]["properties"]["x"]["type"],
            json!("string")
        );
    }

    #[test]
    fn inlines_ref_inside_items_and_chained_refs() {
        let input = json!({
            "type":"object",
            "properties": {"list": {"type":"array","items":{"$ref":"#/$defs/A"}}},
            "$defs": {"A": {"type":"object","properties":{"b":{"$ref":"#/$defs/B"}}},
                      "B": {"type":"object","properties":{"v":{"type":"integer"}}}}
        });
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "$ref"));
        assert_eq!(
            out["properties"]["list"]["items"]["properties"]["b"]["properties"]["v"]["type"],
            json!("integer")
        );
    }

    #[test]
    fn ref_with_sibling_keywords_merges() {
        let input = json!({
            "type":"object",
            "properties": {"p": {"$ref":"#/$defs/T","description":"doc"}},
            "$defs": {"T": {"type":"string"}}
        });
        let out = strict(input);
        assert_eq!(out["properties"]["p"]["type"], json!("string"));
        assert_eq!(out["properties"]["p"]["description"], json!("doc"));
    }

    #[test]
    fn unresolvable_external_ref_becomes_permissive_object() {
        let input = json!({"type":"object","properties":{"p":{"$ref":"https://example/x"}}});
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "$ref"));
        assert_eq!(out["properties"]["p"], json!({"type":"object"}));
    }

    #[test]
    fn recursive_type_terminates_and_is_idempotent() {
        let input = json!({
            "type":"object",
            "properties":{"child":{"$ref":"#/$defs/Node"}},
            "$defs":{"Node":{"type":"object","properties":{"child":{"$ref":"#/$defs/Node"}}}}
        });
        let once = strict(input.clone());
        let twice = rewrite_tool_schema(&once, Ruleset::Strict);
        assert!(!has_key_anywhere(&once, "$ref"));
        assert_eq!(once, twice, "rewrite must be idempotent on recursive types");
    }

    #[test]
    fn collapses_tagged_enum_oneof() {
        // serde adjacently-tagged: {"t": "A"|"B", "c": payload}
        let input = json!({
            "oneOf": [
                {"type":"object","properties":{"t":{"const":"A"},"c":{"type":"object","properties":{"a":{"type":"string"}}}}},
                {"type":"object","properties":{"t":{"const":"B"},"c":{"type":"object","properties":{"b":{"type":"integer"}}}}}
            ]
        });
        let out = strict(input);
        assert!(!has_key_anywhere(&out, "oneOf"));
        assert!(!has_key_anywhere(&out, "anyOf"));
        assert!(!has_key_anywhere(&out, "allOf"));
        assert_eq!(out["type"], json!("object"));
        // tag became an enum of variant tags; properties non-empty
        assert_eq!(out["properties"]["t"]["enum"], json!(["A", "B"]));
        assert!(!out["properties"].as_object().unwrap().is_empty());
        assert!(
            out.get("required").is_none(),
            "Strict must not promote required"
        );
        assert!(
            out.get("additionalProperties").is_none(),
            "Strict must not inject additionalProperties"
        );
    }

    #[test]
    fn strips_unsupported_keywords() {
        let input = json!({"type":"object","$schema":"...","$id":"x","format":"email","examples":[1],
            "properties":{"p":{"type":"string","format":"uri"}}});
        let out = strict(input);
        for k in ["$schema", "$id", "format", "examples"] {
            assert!(!has_key_anywhere(&out, k), "{k} not stripped");
        }
        assert_eq!(out["properties"]["p"]["type"], json!("string"));
    }
}

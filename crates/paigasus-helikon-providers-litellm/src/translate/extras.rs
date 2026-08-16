//! LiteLLM-specific request fields: router controls, observability, and the
//! `extra_body` escape hatch.
//!
//! `tags` nest under `metadata.tags`. All three tag forms (`metadata.tags`,
//! top-level `tags`, and the `x-litellm-tags` header) were measured equivalent
//! on LiteLLM 1.97.0; `metadata.tags` is the form upstream documents as
//! supporting negation (`!`) and required (`&`) prefixes. See the SMA-451
//! design §7.3 and Appendix B.

use serde_json::{Map, Value};

use crate::builder::Extras;

/// Merge the LiteLLM extras into an assembled request body.
///
/// Precedence: `extra_body` wins for the unreserved LiteLLM keys, *except*
/// `metadata`, which is deep-merged so `.metadata()` and an `extra_body`
/// `metadata` object compose rather than clobber. Reserved keys were already
/// rejected at `build()`, so nothing here can overwrite a provider-computed
/// field.
pub(crate) fn apply(body: &mut Map<String, Value>, extras: &Extras) {
    if !extras.fallbacks.is_empty() {
        body.insert(
            "fallbacks".to_owned(),
            Value::from(extras.fallbacks.clone()),
        );
    }
    if let Some(n) = extras.num_retries {
        body.insert("num_retries".to_owned(), Value::from(n));
    }

    let mut metadata = extras.metadata.clone();
    if !extras.tags.is_empty() {
        metadata.insert("tags".to_owned(), Value::from(extras.tags.clone()));
    }
    if !metadata.is_empty() {
        body.insert("metadata".to_owned(), Value::Object(metadata));
    }

    for (k, v) in &extras.extra_body {
        if k == "metadata" {
            match v.as_object() {
                Some(obj) => {
                    // Deep-merge: caller keys win per key, but existing
                    // builder metadata survives.
                    let mut merged = body
                        .get("metadata")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    for (mk, mv) in obj {
                        merged.insert(mk.clone(), mv.clone());
                    }
                    body.insert("metadata".to_owned(), Value::Object(merged));
                }
                None => {
                    // A non-object `extra_body.metadata` (e.g. a string) is
                    // not mergeable. Warn and keep whatever builder
                    // `.metadata()`/`.tags()` already produced rather than
                    // silently clobbering it — `extra_body` stays a usable
                    // escape hatch for the *object* form without being able
                    // to blow away routing/spend tags with no signal.
                    tracing::warn!(
                        target: "paigasus::litellm::translate",
                        "extra_body.metadata must be a JSON object; ignoring non-object value and keeping builder metadata/tags"
                    );
                }
            }
            continue;
        }
        body.insert(k.clone(), v.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn apply_to_empty(extras: Extras) -> Value {
        let mut body = Map::new();
        apply(&mut body, &extras);
        Value::Object(body)
    }

    #[test]
    fn unset_extras_emit_nothing() {
        assert_eq!(apply_to_empty(Extras::default()), json!({}));
    }

    #[test]
    fn fallbacks_and_num_retries_are_top_level() {
        let e = Extras {
            fallbacks: vec!["b".into(), "c".into()],
            num_retries: Some(2),
            ..Default::default()
        };
        let v = apply_to_empty(e);
        assert_eq!(v["fallbacks"], json!(["b", "c"]));
        assert_eq!(v["num_retries"], json!(2));
    }

    #[test]
    fn tags_nest_under_metadata() {
        let e = Extras {
            tags: vec!["team:research".into()],
            ..Default::default()
        };
        let v = apply_to_empty(e);
        assert_eq!(v["metadata"]["tags"], json!(["team:research"]));
        assert!(v.get("tags").is_none(), "tags must not be top-level");
    }

    #[test]
    fn metadata_and_tags_compose() {
        let mut metadata = Map::new();
        metadata.insert("trace_id".into(), json!("t-1"));
        let e = Extras {
            metadata,
            tags: vec!["free".into()],
            ..Default::default()
        };
        let v = apply_to_empty(e);
        assert_eq!(v["metadata"]["trace_id"], "t-1");
        assert_eq!(v["metadata"]["tags"], json!(["free"]));
    }

    #[test]
    fn metadata_accepts_non_string_values() {
        let mut metadata = Map::new();
        metadata.insert("nested".into(), json!({"session_id": 7}));
        let v = apply_to_empty(Extras {
            metadata,
            ..Default::default()
        });
        assert_eq!(v["metadata"]["nested"]["session_id"], 7);
    }

    #[test]
    fn extra_body_merges_at_the_root() {
        let mut extra_body = Map::new();
        extra_body.insert("guardrails".into(), json!(["pii"]));
        let v = apply_to_empty(Extras {
            extra_body,
            ..Default::default()
        });
        assert_eq!(v["guardrails"], json!(["pii"]));
    }

    #[test]
    fn extra_body_metadata_deep_merges_with_builder_metadata_and_tags() {
        let mut metadata = Map::new();
        metadata.insert("trace_id".into(), json!("t-1"));
        let mut extra_body = Map::new();
        extra_body.insert("metadata".into(), json!({"spend_logs_metadata": {"x": 1}}));
        let v = apply_to_empty(Extras {
            metadata,
            tags: vec!["team:research".into()],
            extra_body,
            ..Default::default()
        });
        assert_eq!(
            v["metadata"]["trace_id"], "t-1",
            "builder metadata survives"
        );
        assert_eq!(
            v["metadata"]["tags"],
            json!(["team:research"]),
            "metadata.tags survives a well-formed extra_body.metadata object"
        );
        assert_eq!(v["metadata"]["spend_logs_metadata"]["x"], 1);
    }

    #[test]
    fn extra_body_non_object_metadata_is_ignored_and_builder_metadata_and_tags_survive() {
        // `builder.rs`'s `litellm_extras_are_not_reserved_in_extra_body` test
        // asserts that `.extra_body(json!({"metadata": "x"}))` builds
        // successfully — a non-object `metadata` is a legal build, so `apply`
        // must not let it silently destroy builder-supplied metadata/tags
        // with no signal.
        let mut metadata = Map::new();
        metadata.insert("trace_id".into(), json!("t-1"));
        let mut extra_body = Map::new();
        extra_body.insert("metadata".into(), json!("x"));
        let v = apply_to_empty(Extras {
            metadata,
            tags: vec!["team:research".into()],
            extra_body,
            ..Default::default()
        });
        assert!(
            v["metadata"].is_object(),
            "a non-object extra_body.metadata must not overwrite the metadata object"
        );
        assert_eq!(
            v["metadata"]["trace_id"], "t-1",
            "builder metadata survives a non-object extra_body.metadata"
        );
        assert_eq!(
            v["metadata"]["tags"],
            json!(["team:research"]),
            "metadata.tags survives a non-object extra_body.metadata"
        );
    }

    #[test]
    fn extra_body_can_override_an_unreserved_litellm_extra() {
        // `tags` is deliberately NOT reserved so extra_body stays a usable
        // escape hatch if the metadata.tags shape ever changes upstream.
        let mut extra_body = Map::new();
        extra_body.insert("tags".into(), json!(["top-level"]));
        let v = apply_to_empty(Extras {
            tags: vec!["nested".into()],
            extra_body,
            ..Default::default()
        });
        assert_eq!(v["tags"], json!(["top-level"]));
        assert_eq!(v["metadata"]["tags"], json!(["nested"]));
    }
}

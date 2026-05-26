//! `CacheStrategy` → cache_control marker placement.
//!
//! Anthropic accepts up to 4 cache breakpoints per request. Our placements
//! top out at 3 (system + tools + last turn).

use serde_json::{json, Value};

use crate::settings::CacheStrategy;

/// Apply the cache strategy to the request body in-place.
///
/// `system` is normalized into block-array form when a system marker is
/// requested (Anthropic accepts a string OR an array; markers need the
/// array form). Returns the (possibly converted) system value.
pub(crate) fn apply_cache_strategy(
    strategy: CacheStrategy,
    system: Option<Value>,
    tools: &mut Value,
    messages: &mut Value,
) -> Option<Value> {
    let mark = || json!({"type": "ephemeral"});
    let system = match strategy {
        CacheStrategy::None => system,
        CacheStrategy::System | CacheStrategy::SystemAndTools => system.map(|s| {
            let mut blocks = match s {
                Value::String(text) => vec![json!({"type": "text", "text": text})],
                Value::Array(arr) => arr,
                _ => return s,
            };
            if let Some(last) = blocks.last_mut() {
                if let Some(obj) = last.as_object_mut() {
                    obj.insert("cache_control".to_owned(), mark());
                }
            }
            Value::Array(blocks)
        }),
        CacheStrategy::Tools | CacheStrategy::LastTurn => system,
    };

    if matches!(strategy, CacheStrategy::Tools | CacheStrategy::SystemAndTools) {
        if let Some(arr) = tools.as_array_mut() {
            if let Some(last) = arr.last_mut() {
                if let Some(obj) = last.as_object_mut() {
                    obj.insert("cache_control".to_owned(), mark());
                }
            }
        }
    }

    if matches!(strategy, CacheStrategy::LastTurn) {
        if let Some(arr) = messages.as_array_mut() {
            if let Some(last_msg) = arr.last_mut() {
                if let Some(blocks) = last_msg["content"].as_array_mut() {
                    if let Some(last_block) = blocks.last_mut() {
                        if let Some(obj) = last_block.as_object_mut() {
                            obj.insert("cache_control".to_owned(), mark());
                        }
                    }
                }
            }
        }
    }

    system
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark() -> Value {
        json!({"type": "ephemeral"})
    }

    #[test]
    fn none_strategy_leaves_body_untouched() {
        let mut tools = json!([{"name": "t1"}]);
        let mut messages = json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}]);
        let system = apply_cache_strategy(
            CacheStrategy::None,
            Some(Value::String("S".to_owned())),
            &mut tools,
            &mut messages,
        );
        assert_eq!(system, Some(Value::String("S".to_owned())));
        assert!(tools[0].get("cache_control").is_none());
        assert!(messages[0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn system_strategy_converts_string_to_array_and_marks_last_block() {
        let mut tools = json!([]);
        let mut messages = json!([]);
        let system = apply_cache_strategy(
            CacheStrategy::System,
            Some(Value::String("S".to_owned())),
            &mut tools,
            &mut messages,
        );
        let arr = system.unwrap();
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "S");
        assert_eq!(arr[0]["cache_control"], mark());
    }

    #[test]
    fn tools_strategy_marks_last_tool() {
        let mut tools = json!([{"name": "a"}, {"name": "b"}]);
        let mut messages = json!([]);
        apply_cache_strategy(CacheStrategy::Tools, None, &mut tools, &mut messages);
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"], mark());
    }

    #[test]
    fn system_and_tools_marks_both() {
        let mut tools = json!([{"name": "a"}]);
        let mut messages = json!([]);
        let system = apply_cache_strategy(
            CacheStrategy::SystemAndTools,
            Some(Value::String("S".to_owned())),
            &mut tools,
            &mut messages,
        );
        assert_eq!(system.unwrap()[0]["cache_control"], mark());
        assert_eq!(tools[0]["cache_control"], mark());
    }

    #[test]
    fn empty_system_and_tools_inserts_nothing() {
        let mut tools = json!([]);
        let mut messages = json!([]);
        let system = apply_cache_strategy(
            CacheStrategy::SystemAndTools,
            None,
            &mut tools,
            &mut messages,
        );
        assert!(system.is_none());
        assert_eq!(tools, json!([]));
    }

    #[test]
    fn last_turn_marks_final_block_of_final_message() {
        let mut tools = json!([]);
        let mut messages = json!([
            {"role": "user", "content": [{"type": "text", "text": "first"}]},
            {"role": "user", "content": [{"type": "text", "text": "second"}]},
        ]);
        apply_cache_strategy(CacheStrategy::LastTurn, None, &mut tools, &mut messages);
        assert!(messages[0]["content"][0].get("cache_control").is_none());
        assert_eq!(messages[1]["content"][0]["cache_control"], mark());
    }
}

//! Token estimation for [`crate::CompactingSession`] threshold decisions.

use crate::Item;

/// Estimates the token cost of a projected conversation, so a
/// [`crate::CompactingSession`] can decide when to summarize.
///
/// Pluggable so callers can supply a model-accurate tokenizer; the default
/// [`HeuristicTokenCounter`] is a cheap, deterministic approximation.
pub trait TokenCounter: Send + Sync + std::fmt::Debug {
    /// Estimate the token count of `items`.
    fn count(&self, items: &[Item]) -> usize;
}

/// Default [`TokenCounter`]: `ceil(total_chars / 4)`, where `total_chars`
/// counts Unicode scalar values across every text-bearing field (see the
/// crate docs and spec §4.1 for the exact enumeration). Deterministic; no deps.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicTokenCounter;

use crate::ContentPart;

impl TokenCounter for HeuristicTokenCounter {
    fn count(&self, items: &[Item]) -> usize {
        let chars: usize = items.iter().map(item_chars).sum();
        chars.div_ceil(4)
    }
}

fn item_chars(item: &Item) -> usize {
    match item {
        Item::UserMessage { content }
        | Item::AssistantMessage { content, .. }
        | Item::System { content }
        | Item::ToolResult { content, .. } => content.iter().map(part_chars).sum(),
        Item::ToolCall { name, args, .. } => name.chars().count() + json_chars(args),
    }
}

fn part_chars(part: &ContentPart) -> usize {
    match part {
        ContentPart::Text { text } | ContentPart::Reasoning { text } => text.chars().count(),
        ContentPart::ToolUse { name, args, .. } => name.chars().count() + json_chars(args),
        ContentPart::ToolResult { content, .. } => content.iter().map(part_chars).sum(),
        // Image/Audio sources are not projected text.
        ContentPart::Image { .. } | ContentPart::Audio { .. } => 0,
    }
}

fn json_chars(v: &serde_json::Value) -> usize {
    // Compact JSON length in chars; deterministic across runs.
    serde_json::to_string(v)
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentPart;

    fn text_item(s: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text { text: s.into() }],
        }
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(HeuristicTokenCounter.count(&[]), 0);
    }

    #[test]
    fn counts_chars_div_ceil_four() {
        // 5 chars -> ceil(5/4) = 2
        assert_eq!(HeuristicTokenCounter.count(&[text_item("hello")]), 2);
        // multibyte counted as scalar values, not bytes: "héllo" is 5 chars
        assert_eq!(HeuristicTokenCounter.count(&[text_item("héllo")]), 2);
    }

    #[test]
    fn counts_tool_call_args_and_system_summary() {
        let items = vec![
            Item::System {
                content: vec![ContentPart::Text {
                    text: "summary text".into(),
                }],
            },
            Item::ToolCall {
                call_id: "c".into(),
                name: "calc".into(),
                args: serde_json::json!({"x": 1}),
            },
        ];
        // Non-zero: System content + tool name + args JSON all contribute.
        assert!(HeuristicTokenCounter.count(&items) > 0);
    }
}

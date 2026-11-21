//! Anthropic-specific configuration: prompt-caching strategy + extended thinking.

/// Where to place `cache_control: {type: "ephemeral"}` markers in the request body.
///
/// **Default `None` is opt-out:** no markers, body byte-identical to the
/// uncached path. Anthropic's prompt cache requires a per-model write
/// minimum (~1024 tokens for Sonnet, ~2048 for Opus); below that, the
/// strategy is a documented no-op and the cache simply does not write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheStrategy {
    /// No cache_control markers.
    #[default]
    None,
    /// Mark the final block of `system:` as a cache breakpoint.
    System,
    /// Mark the final tool in `tools[]` as a cache breakpoint.
    Tools,
    /// Mark both system and the last tool.
    SystemAndTools,
    /// Mark the final message in `messages[]` (rolling cache).
    LastTurn,
}

/// Configuration for Anthropic extended/adaptive thinking.
///
/// **Model compatibility:**
/// - `Enabled { .. }` works only on models before Claude 4.6, for example
///   `claude-haiku-4-5`. It is deprecated on Opus 4.6 and Sonnet 4.6.
/// - Claude Opus 4.7 and later, Sonnet 5 and later, Haiku 5.5 and Fable
///   reject `Enabled { .. }` with a 400 error. Use `Adaptive`.
/// - Newer models run a prefix check on each thinking block. The signature
///   records the `system` text, the tools, and all messages before the block.
///
/// **Prefix check.** With `Adaptive`, the provider sends
/// `block_binding.prefix_mismatch_behavior = "drop_block"` and the
/// `thinking-binding-controls-2026-08-01` beta header. Sometimes the history
/// before a thinking block changed. An example is a hand-off with other
/// `system` text or tools. Another example is an edit of an earlier message.
/// Then the API drops that block and every later thinking block. The model
/// loses that reasoning. The request does not fail.
///
/// With `Enabled`, the provider sends no binding. A changed prefix can then
/// fail with a 400 error on a model that runs the check. Use `Enabled` only
/// on models that do not run the check.
///
/// Anthropic requires `budget_tokens < max_tokens` for `Enabled` but
/// documents no absolute minimum; this crate does not enforce one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtendedThinking {
    /// No `thinking` field in the request.
    #[default]
    Disabled,
    /// `thinking: { type: "enabled", budget_tokens: N }`.
    Enabled {
        /// Maximum tokens the model may spend on internal reasoning.
        budget_tokens: u32,
    },
    /// `thinking: { type: "adaptive", block_binding: { prefix_mismatch_behavior: "drop_block" } }`.
    /// The model picks the budget. The provider adds the binding beta header.
    Adaptive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_strategy_default_is_none() {
        assert_eq!(CacheStrategy::default(), CacheStrategy::None);
    }

    #[test]
    fn extended_thinking_default_is_disabled() {
        assert_eq!(ExtendedThinking::default(), ExtendedThinking::Disabled);
    }

    #[test]
    fn extended_thinking_enabled_carries_budget() {
        let t = ExtendedThinking::Enabled {
            budget_tokens: 8192,
        };
        match t {
            ExtendedThinking::Enabled { budget_tokens } => assert_eq!(budget_tokens, 8192),
            _ => panic!("expected Enabled"),
        }
    }
}

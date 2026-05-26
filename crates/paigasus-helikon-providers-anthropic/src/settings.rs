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
/// - Claude Opus 4.7 rejects `Enabled { .. }` (400). Use `Adaptive`.
/// - Sonnet/Opus 4.6 accept both but recommend `Adaptive`.
/// - Older Claude 4 (4.5, 4.1) accept `Enabled` and recommend it.
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
    /// `thinking: { type: "adaptive" }`. Model picks the budget.
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

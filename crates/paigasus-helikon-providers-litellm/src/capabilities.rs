//! Capability defaults for a LiteLLM proxy alias.
//!
//! There is deliberately **no** `KNOWN_MODELS` table: LiteLLM model names are
//! operator-chosen aliases (`prod-fast`, `team-a/gpt`) that carry no
//! information the SDK can act on. See the SMA-451 design §11.

use paigasus_helikon_core::ModelCapabilities;

/// Conservative starting capabilities for an unknown proxied backend.
///
/// `parallel_tool_calls` is intentionally unset: most OpenAI-compatible
/// proxies do not support parallel tool calls, and a loop that expects
/// multiple calls fails worse than one that expects a single call.
pub(crate) const fn conservative_defaults() -> ModelCapabilities {
    ModelCapabilities::empty().with_streaming().with_tools()
}

/// Apply a caller override, forcing `streaming` back on.
///
/// The provider has no non-streaming path, so a caller override that cleared
/// `streaming` would advertise a self-contradictory model.
pub(crate) fn apply_override(
    base: ModelCapabilities,
    over: Option<ModelCapabilities>,
) -> ModelCapabilities {
    let mut caps = over.unwrap_or(base);
    caps.streaming = true;
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_streaming_and_tools_only() {
        let c = conservative_defaults();
        assert!(c.streaming);
        assert!(c.tools);
        assert!(!c.parallel_tool_calls, "conservative default must be false");
        assert!(!c.structured_output);
        assert!(!c.vision);
        assert!(!c.reasoning);
        assert!(!c.server_managed_state);
    }

    #[test]
    fn override_replaces_the_default_set() {
        let over = ModelCapabilities::empty().with_tools().with_vision();
        let c = apply_override(conservative_defaults(), Some(over));
        assert!(c.vision, "override must win over the default");
        assert!(c.tools);
    }

    #[test]
    fn override_cannot_clear_streaming() {
        // The provider has no non-streaming path.
        let over = ModelCapabilities::empty().with_tools();
        let c = apply_override(conservative_defaults(), Some(over));
        assert!(c.streaming, "streaming must be forced back on");
    }

    #[test]
    fn no_override_returns_the_base() {
        let c = apply_override(conservative_defaults(), None);
        assert_eq!(c, conservative_defaults());
    }
}

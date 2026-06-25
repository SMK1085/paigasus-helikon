//! Model capability flags for Bedrock model families.
//!
//! Returns conservative defaults for unknown families; callers may override
//! via [`crate::BedrockModelBuilder::capabilities`].

use crate::family::ModelFamily;
use paigasus_helikon_core::ModelCapabilities;

/// Return the default `(capabilities, max_output_tokens)` pair for a family.
///
/// The `structured_output` flag is set for families that support forced
/// tool-choice (the mechanism used to synthesize structured output on Bedrock).
/// The `max_output` value is a conservative default; the builder lets callers
/// override it.
pub(crate) fn caps_for(family: ModelFamily) -> (ModelCapabilities, u32) {
    match family {
        ModelFamily::Anthropic => (
            ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision(),
            8_192,
        ),
        ModelFamily::AmazonNova => (
            ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision(),
            5_120,
        ),
        ModelFamily::AmazonTitan => (
            ModelCapabilities::empty().with_streaming().with_tools(),
            8_192,
        ),
        ModelFamily::Llama => (
            ModelCapabilities::empty().with_streaming().with_tools(),
            8_192,
        ),
        ModelFamily::Mistral => (
            ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output(),
            8_192,
        ),
        ModelFamily::Cohere => (
            ModelCapabilities::empty().with_streaming().with_tools(),
            4_096,
        ),
        ModelFamily::Unknown => (
            ModelCapabilities::empty().with_streaming().with_tools(),
            4_096,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::family::ModelFamily;

    #[test]
    fn anthropic_has_full_capabilities() {
        let (caps, max_out) = caps_for(ModelFamily::Anthropic);
        assert!(caps.streaming, "anthropic: streaming");
        assert!(caps.tools, "anthropic: tools");
        assert!(caps.parallel_tool_calls, "anthropic: parallel");
        assert!(caps.structured_output, "anthropic: structured_output");
        assert!(caps.vision, "anthropic: vision");
        assert!(max_out > 0, "anthropic: max_out > 0");
    }

    #[test]
    fn llama_has_streaming_and_tools_but_not_structured_output() {
        let (caps, max_out) = caps_for(ModelFamily::Llama);
        assert!(caps.streaming, "llama: streaming");
        assert!(caps.tools, "llama: tools");
        assert!(!caps.structured_output, "llama: no structured_output");
        assert!(max_out > 0, "llama: max_out > 0");
    }

    #[test]
    fn unknown_has_conservative_defaults() {
        let (caps, max_out) = caps_for(ModelFamily::Unknown);
        assert!(caps.streaming, "unknown: streaming");
        assert!(caps.tools, "unknown: tools");
        assert!(!caps.structured_output, "unknown: no structured_output");
        assert!(!caps.vision, "unknown: no vision");
        assert_eq!(max_out, 4_096, "unknown: max_out 4096");
    }

    #[test]
    fn amazon_nova_supports_structured_output() {
        let (caps, _) = caps_for(ModelFamily::AmazonNova);
        assert!(caps.structured_output, "nova: structured_output");
        assert!(caps.parallel_tool_calls, "nova: parallel");
        assert!(caps.vision, "nova: vision");
    }

    #[test]
    fn amazon_titan_does_not_support_structured_output() {
        let (caps, max_out) = caps_for(ModelFamily::AmazonTitan);
        assert!(caps.streaming);
        assert!(caps.tools);
        assert!(!caps.structured_output);
        assert!(max_out > 0);
    }

    #[test]
    fn structured_output_matches_forced_tool_choice_support() {
        // structured_output flag should align with supports_forced_tool_choice
        for family in [
            ModelFamily::Anthropic,
            ModelFamily::AmazonNova,
            ModelFamily::AmazonTitan,
            ModelFamily::Llama,
            ModelFamily::Mistral,
            ModelFamily::Cohere,
            ModelFamily::Unknown,
        ] {
            let (caps, _) = caps_for(family);
            assert_eq!(
                caps.structured_output,
                family.supports_forced_tool_choice(),
                "family {family:?}: structured_output != supports_forced_tool_choice"
            );
        }
    }
}

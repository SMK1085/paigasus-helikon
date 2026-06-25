//! Model capability flags for Bedrock model families.
//!
//! Returns conservative defaults for unknown families; callers may override
//! via [`crate::BedrockModelBuilder::capabilities`].

use crate::family::ModelFamily;
use paigasus_helikon_core::ModelCapabilities;

/// Return the capability flags for a family.
///
/// The `structured_output` flag is set for families that support forced
/// tool-choice (the mechanism used to synthesize structured output on Bedrock).
/// No per-family `max_output_tokens` default is returned; omitting
/// `inferenceConfig.maxTokens` lets the model apply its own correct default,
/// which avoids `ValidationException` on models whose limit is below any
/// hardcoded value.  Callers may supply an explicit default via
/// [`crate::BedrockModelBuilder::max_output_tokens_default`].
pub(crate) fn caps_for(family: ModelFamily) -> ModelCapabilities {
    match family {
        ModelFamily::Anthropic => ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_parallel_tool_calls()
            .with_structured_output()
            .with_vision(),
        ModelFamily::AmazonNova => ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_parallel_tool_calls()
            .with_structured_output()
            .with_vision(),
        ModelFamily::AmazonTitan => ModelCapabilities::empty().with_streaming().with_tools(),
        ModelFamily::Llama => ModelCapabilities::empty().with_streaming().with_tools(),
        ModelFamily::Mistral => ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_parallel_tool_calls()
            .with_structured_output(),
        ModelFamily::Cohere => ModelCapabilities::empty().with_streaming().with_tools(),
        ModelFamily::Unknown => ModelCapabilities::empty().with_streaming().with_tools(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::family::ModelFamily;

    #[test]
    fn anthropic_has_full_capabilities() {
        let caps = caps_for(ModelFamily::Anthropic);
        assert!(caps.streaming, "anthropic: streaming");
        assert!(caps.tools, "anthropic: tools");
        assert!(caps.parallel_tool_calls, "anthropic: parallel");
        assert!(caps.structured_output, "anthropic: structured_output");
        assert!(caps.vision, "anthropic: vision");
    }

    #[test]
    fn llama_has_streaming_and_tools_but_not_structured_output() {
        let caps = caps_for(ModelFamily::Llama);
        assert!(caps.streaming, "llama: streaming");
        assert!(caps.tools, "llama: tools");
        assert!(!caps.structured_output, "llama: no structured_output");
    }

    #[test]
    fn unknown_has_conservative_defaults() {
        let caps = caps_for(ModelFamily::Unknown);
        assert!(caps.streaming, "unknown: streaming");
        assert!(caps.tools, "unknown: tools");
        assert!(!caps.structured_output, "unknown: no structured_output");
        assert!(!caps.vision, "unknown: no vision");
    }

    #[test]
    fn amazon_nova_supports_structured_output() {
        let caps = caps_for(ModelFamily::AmazonNova);
        assert!(caps.structured_output, "nova: structured_output");
        assert!(caps.parallel_tool_calls, "nova: parallel");
        assert!(caps.vision, "nova: vision");
    }

    #[test]
    fn amazon_titan_does_not_support_structured_output() {
        let caps = caps_for(ModelFamily::AmazonTitan);
        assert!(caps.streaming);
        assert!(caps.tools);
        assert!(!caps.structured_output);
    }

    #[test]
    fn mistral_supports_parallel_tool_calls() {
        let caps = caps_for(ModelFamily::Mistral);
        assert!(caps.parallel_tool_calls, "mistral: parallel_tool_calls");
        assert!(caps.structured_output, "mistral: structured_output");
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
            let caps = caps_for(family);
            assert_eq!(
                caps.structured_output,
                family.supports_forced_tool_choice(),
                "family {family:?}: structured_output != supports_forced_tool_choice"
            );
        }
    }
}

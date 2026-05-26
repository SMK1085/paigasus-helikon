//! KNOWN_MODELS capability lookup for Anthropic models.
//!
//! Hardcoded table per the SMA-317 spec. Anthropic exposes no
//! machine-readable capability manifest. Unknown ids fall through to
//! conservative defaults. Callers can override via
//! [`crate::AnthropicModelBuilder::with_capabilities`].

use paigasus_helikon_core::ModelCapabilities;

/// Capability + default-output-token snapshot for a model id.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelEntry {
    pub(crate) caps: ModelCapabilities,
    pub(crate) max_output_default: u32,
}

/// Conservative fallback for ids absent from [`KNOWN_MODELS`].
pub(crate) const fn conservative_defaults() -> ModelEntry {
    ModelEntry {
        caps: ModelCapabilities::empty().with_streaming().with_tools(),
        max_output_default: 4096,
    }
}

/// Capability snapshot keyed by exact model id.
///
/// Cross-check entries against Anthropic's published model docs at
/// implementation time. Entries that diverge are bugs — file follow-up
/// chore-PRs to keep this table aligned with reality.
pub(crate) const KNOWN_MODELS: &[(&str, ModelEntry)] = &[
    // Claude 4 family — primary lineup as of 2026-05.
    (
        "claude-opus-4-7",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-opus-4-6",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-opus-4-5",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-opus-4-1",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-sonnet-4-6",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-sonnet-4-5",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-haiku-4-5",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    // Claude 3.5 family
    (
        "claude-3-5-sonnet-latest",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    (
        "claude-3-5-sonnet-20241022",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    (
        "claude-3-5-haiku-latest",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    (
        "claude-3-5-haiku-20241022",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    // Claude 3 family — older; URL-form image inputs may 400 (use base64).
    (
        "claude-3-opus-latest",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 4096,
        },
    ),
    (
        "claude-3-opus-20240229",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 4096,
        },
    ),
    (
        "claude-3-sonnet-20240229",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision(),
            max_output_default: 4096,
        },
    ),
    (
        "claude-3-haiku-20240307",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision(),
            max_output_default: 4096,
        },
    ),
];

/// Look up the capability + default-output snapshot for a model id.
pub(crate) fn lookup(model_id: &str) -> ModelEntry {
    KNOWN_MODELS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, e)| *e)
        .unwrap_or_else(conservative_defaults)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_have_no_duplicate_ids() {
        let mut ids: Vec<&str> = KNOWN_MODELS.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "duplicate id in KNOWN_MODELS");
    }

    #[test]
    fn opus_4_7_advertises_reasoning_and_caching() {
        let e = lookup("claude-opus-4-7");
        assert!(e.caps.reasoning);
        assert!(e.caps.prompt_caching);
        assert!(e.caps.parallel_tool_calls);
        assert!(e.caps.vision);
        assert_eq!(e.max_output_default, 32_768);
    }

    #[test]
    fn haiku_3_5_has_no_vision() {
        let e = lookup("claude-3-5-haiku-20241022");
        assert!(!e.caps.vision, "3.5 Haiku has no vision");
        assert!(e.caps.prompt_caching);
    }

    #[test]
    fn old_3_sonnet_lacks_prompt_caching() {
        let e = lookup("claude-3-sonnet-20240229");
        assert!(!e.caps.prompt_caching);
        assert!(e.caps.vision);
    }

    #[test]
    fn unknown_id_falls_through_to_conservative_defaults() {
        let e = lookup("claude-mystery-9000");
        assert!(e.caps.streaming);
        assert!(e.caps.tools);
        assert!(!e.caps.parallel_tool_calls);
        assert!(!e.caps.structured_output);
        assert!(!e.caps.vision);
        assert!(!e.caps.reasoning);
        assert!(!e.caps.prompt_caching);
        assert_eq!(e.max_output_default, 4096);
    }
}

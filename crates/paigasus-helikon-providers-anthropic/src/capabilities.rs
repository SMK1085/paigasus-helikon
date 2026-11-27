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
    // Claude 4 and 5 families.
    (
        "claude-fable-5-2",
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
        "claude-fable-5-1",
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
        "claude-fable-5",
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
        "claude-opus-5-5",
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
        "claude-opus-5",
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
        "claude-opus-4-8",
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
        "claude-sonnet-5-5",
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
        "claude-sonnet-5",
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
        "claude-haiku-5-5",
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
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 8192,
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
    fn haiku_5_5_row_matches_the_observed_capabilities() {
        let e = lookup("claude-haiku-5-5");
        assert!(e.caps.streaming);
        assert!(e.caps.tools);
        assert!(e.caps.parallel_tool_calls);
        assert!(e.caps.structured_output);
        assert!(e.caps.vision);
        assert!(e.caps.reasoning);
        assert!(e.caps.prompt_caching);
        assert_eq!(e.max_output_default, 32_768);
    }

    /// Every current Claude 5 id, and Opus 4.8, reports the full set. The
    /// Models API lists vision, structured outputs and thinking for each.
    #[test]
    fn current_claude_5_rows_report_the_full_set() {
        for id in [
            "claude-fable-5-2",
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-sonnet-5-5",
            "claude-sonnet-5",
        ] {
            let e = lookup(id);
            assert!(e.caps.streaming, "{id}");
            assert!(e.caps.tools, "{id}");
            assert!(e.caps.parallel_tool_calls, "{id}");
            assert!(e.caps.structured_output, "{id}");
            assert!(e.caps.vision, "{id}");
            assert!(e.caps.reasoning, "{id}");
            assert!(e.caps.prompt_caching, "{id}");
            assert_eq!(e.max_output_default, 32_768, "{id}");
        }
    }

    /// Haiku 4.5 accepts `ExtendedThinking::Enabled`, so it reports reasoning.
    #[test]
    fn haiku_4_5_reports_reasoning() {
        let e = lookup("claude-haiku-4-5");
        assert!(e.caps.reasoning);
        assert_eq!(e.max_output_default, 8192);
    }

    /// Claude Opus 4.1 is retired. Its id gets the conservative set.
    #[test]
    fn retired_opus_4_1_has_no_row() {
        assert!(KNOWN_MODELS.iter().all(|(id, _)| *id != "claude-opus-4-1"));
        assert_eq!(lookup("claude-opus-4-1").max_output_default, 4096);
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

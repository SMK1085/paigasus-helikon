//! KNOWN_MODELS capability lookup.
//!
//! Hardcoded table per [SMA-316 spec § Capabilities] — OpenAI exposes no
//! machine-readable capability manifest. Unknown ids fall through to
//! conservative defaults; callers can override via
//! [`crate::OpenAiModelBuilder::with_capabilities`].
//!
//! [SMA-316 spec § Capabilities]: ../../../../docs/superpowers/specs/2026-05-26-sma-316-openai-provider-design.md

use paigasus_helikon_core::ModelCapabilities;

/// Which OpenAI endpoint family a model targets.
///
/// Crate-internal because it lives on the `OpenAiModel`'s
/// backend-dispatch surface, not the public builder API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // used by D2 (OpenAiModel backend dispatch)
pub(crate) enum Backend {
    /// Chat Completions (`/v1/chat/completions`).
    Chat,
    /// Responses API (`/v1/responses`).
    Responses,
}

/// Conservative capability defaults for ids absent from [`KNOWN_MODELS`].
///
/// `parallel_tool_calls` is intentionally `false` — most OpenAI-compatible
/// proxies (vLLM, LiteLLM, Ollama, llama.cpp) don't support parallel tool
/// calls, and a loop that expects multiple-call responses fails worse than
/// one that expects single-call.
#[allow(dead_code)] // used by lookup(); lookup() used by D2
pub(crate) const fn conservative_defaults() -> ModelCapabilities {
    ModelCapabilities::empty()
        .with_streaming()
        .with_tools()
    // parallel_tool_calls intentionally not set — see doc comment
}

/// Capability snapshot keyed by exact model id.
///
/// Cross-check entries against OpenAI's published model docs at
/// implementation time. Entries that diverge are bugs — file follow-up
/// chore-PRs to keep this table aligned with reality.
#[allow(dead_code)] // used by lookup(); lookup() used by D2
pub(crate) const KNOWN_MODELS: &[(&str, ModelCapabilities)] = &[
    // Chat Completions family
    ("gpt-4o", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()
        .with_structured_output().with_vision()),
    ("gpt-4o-mini", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()
        .with_structured_output().with_vision()),
    ("gpt-4.1", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()
        .with_structured_output().with_vision()),
    ("gpt-4.1-mini", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()
        .with_structured_output().with_vision()),
    ("gpt-3.5-turbo", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()),

    // Responses-family reasoning models. server_managed_state /
    // reasoning are masked off when paired with Backend::Chat in
    // `mask_for_backend`.
    ("o1", ModelCapabilities::empty()
        .with_streaming().with_tools()
        .with_structured_output().with_server_managed_state().with_reasoning()),
    ("o1-mini", ModelCapabilities::empty()
        .with_streaming().with_tools()
        .with_structured_output().with_server_managed_state().with_reasoning()),
    ("o3", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()
        .with_structured_output().with_server_managed_state().with_reasoning()),
    ("o3-mini", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()
        .with_structured_output().with_server_managed_state().with_reasoning()),
    ("gpt-5", ModelCapabilities::empty()
        .with_streaming().with_tools().with_parallel_tool_calls()
        .with_structured_output().with_server_managed_state().with_reasoning().with_vision()),
];

/// Look up the capability snapshot for a model id.
///
/// Returns the [`KNOWN_MODELS`] entry when present, else
/// [`conservative_defaults`]. Callers apply [`mask_for_backend`] after
/// this to clear Responses-only capabilities when the caller chose the
/// Chat backend.
#[allow(dead_code)] // used by D2 (OpenAiModel backend dispatch)
pub(crate) fn lookup(model_id: &str) -> ModelCapabilities {
    KNOWN_MODELS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, caps)| *caps)
        .unwrap_or_else(conservative_defaults)
}

/// Mask off capabilities that don't make sense for the chosen backend.
///
/// `server_managed_state` and `reasoning` are Responses-API features;
/// they get cleared when paired with [`Backend::Chat`]. Forwards-compatible:
/// add new masking rules here when future capabilities turn out to be
/// backend-specific.
#[allow(dead_code)] // used by D2 (OpenAiModel backend dispatch)
pub(crate) fn mask_for_backend(
    mut caps: ModelCapabilities,
    backend: Backend,
) -> ModelCapabilities {
    match backend {
        Backend::Chat => {
            caps.server_managed_state = false;
            caps.reasoning = false;
            caps
        }
        Backend::Responses => caps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_id_returns_table_entry() {
        let caps = lookup("gpt-4o");
        assert!(caps.streaming);
        assert!(caps.tools);
        assert!(caps.parallel_tool_calls);
        assert!(caps.vision);
        assert!(caps.structured_output);
    }

    #[test]
    fn lookup_unknown_id_returns_conservative_defaults() {
        let caps = lookup("some-mystery-model-9000");
        assert!(caps.streaming);
        assert!(caps.tools);
        assert!(!caps.parallel_tool_calls, "conservative default must be false");
        assert!(!caps.structured_output);
        assert!(!caps.server_managed_state);
        assert!(!caps.reasoning);
        assert!(!caps.vision);
        assert!(!caps.audio);
    }

    #[test]
    fn mask_for_chat_backend_clears_responses_only_capabilities() {
        // server_managed_state=true, reasoning=true, all others false
        let raw = ModelCapabilities::empty()
            .with_server_managed_state()
            .with_reasoning();
        let masked = mask_for_backend(raw, Backend::Chat);
        assert!(!masked.server_managed_state);
        assert!(!masked.reasoning);
    }

    #[test]
    fn mask_for_responses_backend_preserves_responses_only_capabilities() {
        // server_managed_state=true, reasoning=true, all others false
        let raw = ModelCapabilities::empty()
            .with_server_managed_state()
            .with_reasoning();
        let masked = mask_for_backend(raw, Backend::Responses);
        assert!(masked.server_managed_state);
        assert!(masked.reasoning);
    }

    #[test]
    fn known_models_table_has_no_duplicates() {
        let mut ids: Vec<&str> = KNOWN_MODELS.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        let len_before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len_before, "KNOWN_MODELS has duplicate ids");
    }
}

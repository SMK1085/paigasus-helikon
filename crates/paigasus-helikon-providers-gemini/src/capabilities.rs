//! KNOWN_MODELS capability lookup for Gemini models.

use paigasus_helikon_core::ModelCapabilities;

/// Capability snapshot for a model id.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelEntry {
    pub(crate) caps: ModelCapabilities,
}

/// Conservative fallback for ids absent from [`KNOWN_MODELS`].
pub(crate) const fn conservative_defaults() -> ModelEntry {
    ModelEntry {
        caps: ModelCapabilities::empty().with_streaming().with_tools(),
    }
}

const fn full() -> ModelEntry {
    ModelEntry {
        caps: ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_parallel_tool_calls()
            .with_structured_output()
            .with_vision(),
    }
}

/// Capability snapshot keyed by exact model id. Cross-check against Google's
/// published model docs at implementation time; divergences are bugs.
pub(crate) const KNOWN_MODELS: &[(&str, ModelEntry)] = &[
    ("gemini-2.5-pro", full()),
    ("gemini-2.5-flash", full()),
    ("gemini-2.0-flash", full()),
    ("gemini-2.0-flash-lite", full()),
];

/// Look up capabilities for `model_id`, falling back to conservative defaults.
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
    fn known_25_flash_has_tools_and_structured_output() {
        let e = lookup("gemini-2.5-flash");
        assert!(e.caps.streaming && e.caps.tools && e.caps.structured_output && e.caps.vision);
        assert!(e.caps.parallel_tool_calls);
        // Reasoning streaming deferred (D3): flag stays false even for 2.5.
        assert!(!e.caps.reasoning);
    }

    #[test]
    fn unknown_model_falls_back_to_conservative() {
        let e = lookup("gemini-9-ultra");
        assert!(e.caps.streaming && e.caps.tools);
        assert!(!e.caps.structured_output);
    }
}

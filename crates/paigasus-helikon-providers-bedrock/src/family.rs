//! Bedrock model family detection and capability routing.

/// Identifies the model family of a Bedrock Converse model ID.
///
/// Used internally to select capability flags, schema rewriter rulesets, and
/// tool-choice support. The `#[non_exhaustive]` attribute allows future
/// additions without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelFamily {
    /// Anthropic Claude model family (e.g. `anthropic.claude-*`).
    Anthropic,
    /// Amazon Nova model family (e.g. `amazon.nova-*`).
    AmazonNova,
    /// Amazon Titan model family (e.g. `amazon.titan-*`).
    AmazonTitan,
    /// Meta Llama model family (e.g. `meta.llama*`).
    Llama,
    /// Mistral model family (e.g. `mistral.mistral-*`).
    Mistral,
    /// Cohere Command model family (e.g. `cohere.command-*`).
    Cohere,
    /// Unknown or unrecognized model family.
    Unknown,
}

impl ModelFamily {
    /// Detect the model family from a Bedrock model ID.
    ///
    /// Strips a leading cross-region inference profile prefix (`us.`, `eu.`,
    /// `ap.`) before matching on the provider segment.
    ///
    /// # Examples
    ///
    /// ```
    /// use paigasus_helikon_providers_bedrock::ModelFamily;
    /// assert_eq!(
    ///     ModelFamily::from_model_id("anthropic.claude-3-5-sonnet-20241022-v2:0"),
    ///     ModelFamily::Anthropic,
    /// );
    /// ```
    pub fn from_model_id(model_id: &str) -> Self {
        // Strip known cross-region inference profile prefixes.
        let id = strip_region_prefix(model_id);

        // Split on `.` to get the provider segment and the rest.
        let mut parts = id.splitn(2, '.');
        let provider = parts.next().unwrap_or("").to_ascii_lowercase();
        let rest = parts.next().unwrap_or("").to_ascii_lowercase();

        match provider.as_str() {
            "anthropic" => Self::Anthropic,
            "amazon" => {
                if rest.starts_with("nova") {
                    Self::AmazonNova
                } else if rest.starts_with("titan") {
                    Self::AmazonTitan
                } else {
                    Self::Unknown
                }
            }
            "meta" => Self::Llama,
            "mistral" => Self::Mistral,
            "cohere" => Self::Cohere,
            other => {
                // Also detect llama by name in provider segment (some IDs put llama first).
                if other.contains("llama") {
                    Self::Llama
                } else {
                    Self::Unknown
                }
            }
        }
    }

    /// Returns `true` when the family supports Bedrock's forced-tool-choice
    /// (`toolChoice: { tool: { name } }`) request field.
    ///
    /// Families that do not support it will have the `tool_choice` field
    /// omitted and a `debug!` log emitted instead.
    // Used by capabilities.rs, translate/mod.rs (Tasks 5, 9) — allow dead_code
    // until those tasks land.
    #[allow(dead_code)]
    pub(crate) fn supports_forced_tool_choice(self) -> bool {
        matches!(self, Self::Anthropic | Self::Mistral | Self::AmazonNova)
    }
}

/// Strip a leading cross-region inference profile prefix such as `us.`,
/// `eu.`, `ap.`, or `apac.`.
fn strip_region_prefix(id: &str) -> &str {
    for prefix in ["us.", "eu.", "ap.", "apac."] {
        if let Some(rest) = id.strip_prefix(prefix) {
            return rest;
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::ModelFamily::*;
    use super::*;

    #[test]
    fn detects_families_from_bedrock_model_ids() {
        for (id, want) in [
            ("anthropic.claude-3-5-sonnet-20241022-v2:0", Anthropic),
            ("us.anthropic.claude-3-7-sonnet-20250219-v1:0", Anthropic), // cross-region inference profile prefix
            ("amazon.nova-pro-v1:0", AmazonNova),
            ("amazon.titan-text-express-v1", AmazonTitan),
            ("meta.llama3-1-70b-instruct-v1:0", Llama),
            ("mistral.mistral-large-2407-v1:0", Mistral),
            ("cohere.command-r-plus-v1:0", Cohere),
            ("some.future-model", Unknown),
        ] {
            assert_eq!(ModelFamily::from_model_id(id), want, "id={id}");
        }
    }

    #[test]
    fn forced_tool_choice_support() {
        assert!(Anthropic.supports_forced_tool_choice());
        assert!(Mistral.supports_forced_tool_choice());
        assert!(AmazonNova.supports_forced_tool_choice());
        assert!(!Llama.supports_forced_tool_choice());
        assert!(!AmazonTitan.supports_forced_tool_choice());
    }
}

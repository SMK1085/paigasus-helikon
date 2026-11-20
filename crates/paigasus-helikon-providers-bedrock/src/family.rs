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
    /// When the first segment is not a known provider name, detection reads
    /// the second segment. So a geographic prefix such as `us.`, `eu.`,
    /// `apac.`, `jp.`, `au.`, `ca.`, `in.` or `global.` is skipped, and a
    /// future prefix is skipped too. An ARN is classified by its resource ID
    /// (the text after the last `/`). Opaque IDs, such as application
    /// inference profile ARNs, give [`ModelFamily::Unknown`].
    ///
    /// # Examples
    ///
    /// ```
    /// use paigasus_helikon_providers_bedrock::ModelFamily;
    /// assert_eq!(
    ///     ModelFamily::from_model_id("anthropic.claude-3-5-sonnet-20241022-v2:0"),
    ///     ModelFamily::Anthropic,
    /// );
    /// assert_eq!(
    ///     ModelFamily::from_model_id("global.anthropic.claude-sonnet-5-5"),
    ///     ModelFamily::Anthropic,
    /// );
    /// ```
    pub fn from_model_id(model_id: &str) -> Self {
        let lowered = model_id.to_ascii_lowercase();
        let mut id = lowered.as_str();
        if id.starts_with("arn:") {
            match id.rsplit_once('/') {
                Some((_, resource)) => id = resource,
                None => return Self::Unknown,
            }
        }

        let (first, rest1) = id.split_once('.').unwrap_or((id, ""));
        let (second, rest2) = rest1.split_once('.').unwrap_or((rest1, ""));

        Self::classify(first, rest1)
            .or_else(|| Self::classify(second, rest2))
            .unwrap_or(if first.contains("llama") || second.contains("llama") {
                Self::Llama
            } else {
                Self::Unknown
            })
    }

    /// Map a provider segment (and the text after it) to a family, or `None`
    /// when the segment is not a known provider name.
    fn classify(provider: &str, rest: &str) -> Option<Self> {
        match provider {
            "anthropic" => Some(Self::Anthropic),
            "amazon" => Some(if rest.starts_with("nova") {
                Self::AmazonNova
            } else if rest.starts_with("titan") {
                Self::AmazonTitan
            } else {
                Self::Unknown
            }),
            "meta" => Some(Self::Llama),
            "mistral" => Some(Self::Mistral),
            "cohere" => Some(Self::Cohere),
            _ => None,
        }
    }

    /// Returns `true` when the family supports Bedrock's forced-tool-choice
    /// (`toolChoice: { tool: { name } }`) request field.
    ///
    /// Families that do not support it will have the `tool_choice` field
    /// omitted and a `debug!` log emitted instead.
    pub(crate) fn supports_forced_tool_choice(self) -> bool {
        matches!(self, Self::Anthropic | Self::Mistral | Self::AmazonNova)
    }
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
            ("apac.anthropic.claude-3-5-sonnet-20241022-v2:0", Anthropic), // apac prefix
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
    fn detects_family_behind_any_leading_segment() {
        for (id, want) in [
            ("us.anthropic.claude-sonnet-4-5-20250929-v1:0", Anthropic),
            ("eu.anthropic.claude-haiku-4-5-20251001-v1:0", Anthropic),
            ("apac.amazon.nova-pro-v1:0", AmazonNova),
            ("jp.anthropic.claude-sonnet-4-5-20250929-v1:0", Anthropic),
            ("au.anthropic.claude-sonnet-5-5", Anthropic),
            ("ca.anthropic.claude-sonnet-5-5", Anthropic),
            ("in.anthropic.claude-haiku-4-5-20251001-v1:0", Anthropic),
            ("global.anthropic.claude-sonnet-4-5-20250929-v1:0", Anthropic),
            ("global.anthropic.claude-sonnet-5-5", Anthropic),
            ("ap.anthropic.claude-x", Anthropic),
            ("us-gov.anthropic.claude-3-5-sonnet-20240620-v1:0", Anthropic),
            ("global.amazon.nova-2-lite-v1:0", AmazonNova),
            ("us.meta.llama3-2-90b-instruct-v1:0", Llama),
            ("eu.mistral.pixtral-large-2502-v1:0", Mistral),
            ("zz.anthropic.claude-x", Anthropic),
            ("us.deepseek.r1-v1:0", Unknown),
            ("us.writer.palmyra-x5-v1:0", Unknown),
            ("amazon.anthropic.x", Unknown),
            ("us.llama3-x", Llama),
            ("GLOBAL.Anthropic.claude-x", Anthropic),
            ("US.anthropic.x", Anthropic),
            ("", Unknown),
            (".", Unknown),
            ("..anthropic.x", Unknown),
            ("global.", Unknown),
            ("global.anthropic", Anthropic),
            ("arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-3-5-sonnet-20241022-v2:0", Anthropic),
            ("ARN:aws:bedrock:us-east-1::foundation-model/anthropic.claude-x", Anthropic),
            ("arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-3-5-sonnet-20241022-v2:0", Anthropic),
            ("arn:aws:bedrock:us-east-1:123456789012:default-prompt-router/anthropic.claude:1", Anthropic),
            ("arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3d4e5f6", Unknown),
            ("arn:aws:bedrock:us-east-1:123456789012:inference-profile/", Unknown),
            ("arn:aws:bedrock", Unknown),
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

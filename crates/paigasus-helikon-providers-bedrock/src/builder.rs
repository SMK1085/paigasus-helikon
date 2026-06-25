//! [`BedrockModelBuilder`] — fluent constructor for [`crate::BedrockModel`].
//!
//! ## Credentials / AWS config laziness
//!
//! The synchronous [`BedrockModelBuilder::build`] only validates the model-id
//! and assembles a Bedrock SDK [`Client`]; it **does not** make any network
//! call and **does not** verify your AWS credentials.  Credential/auth
//! failures surface later, at invoke time, as a
//! [`paigasus_helikon_core::ModelError`].
//!
//! [`BedrockModel::from_env`] loads the SDK config from the environment (an
//! `async` operation because it may fetch IMDS/SSO tokens), but the same
//! credential-laziness rule applies: loading the config does not prove the
//! credentials will be accepted by Bedrock.

use std::sync::Arc;

use aws_config::Region;
use aws_sdk_bedrockruntime::Client;
use paigasus_helikon_core::ModelCapabilities;

use crate::capabilities::caps_for;
use crate::family::ModelFamily;

// ── BuildError ────────────────────────────────────────────────────────────────

/// Errors that can occur while building a [`crate::BedrockModel`].
///
/// Runtime errors (auth failures, throttling, …) are reported through
/// [`paigasus_helikon_core::ModelError`] at invoke time, not here.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// No AWS SDK client or `SdkConfig` was supplied and `from_env` was not
    /// used.  Call `.client(…)`, `.sdk_config(…)`, or use
    /// [`crate::BedrockModel::from_env`].
    #[error("no AWS client or SdkConfig provided")]
    MissingClient,

    /// The model identifier was empty.  Pass a non-empty Bedrock model id such
    /// as `"anthropic.claude-3-5-sonnet-20241022-v2:0"`.
    #[error("model id is empty")]
    EmptyModelId,
}

// ── Config ────────────────────────────────────────────────────────────────────

/// Resolved builder configuration held by [`crate::BedrockModel`].
///
/// Cheaply clonable via `Arc` — the `BedrockModel` holds `Arc<Config>` so
/// that clone + move into the `'static` stream is zero-copy.
#[derive(Debug, Clone)]
pub(crate) struct Config {
    /// Initialized Bedrock SDK client (region already baked in).
    pub(crate) client: Client,
    /// Bedrock model identifier (e.g. `anthropic.claude-3-5-sonnet-20241022-v2:0`).
    pub(crate) model_id: String,
    /// Detected model family used for capability routing.
    pub(crate) family: ModelFamily,
    /// Resolved capability flags for this model instance.
    pub(crate) capabilities: ModelCapabilities,
    /// Optional default for `max_tokens` when the caller does not set it.
    ///
    /// When `None` (the default), `inferenceConfig.maxTokens` is omitted from
    /// the Bedrock request so that each model applies its own correct limit.
    /// Set via [`BedrockModelBuilder::max_output_tokens_default`].
    pub(crate) max_output_default: Option<u32>,
}

// ── BedrockModelBuilder ───────────────────────────────────────────────────────

/// Fluent builder for [`crate::BedrockModel`].
///
/// Start with [`crate::BedrockModel::converse`].
///
/// ## Credential laziness
///
/// `build()` is **synchronous** and **does not** contact any AWS endpoint.
/// Credential resolution happens inside the SDK client at the first
/// invoke call.
pub struct BedrockModelBuilder {
    model_id: String,
    /// Caller-supplied, fully-initialized client (highest precedence).
    client: Option<Client>,
    /// Caller-supplied `SdkConfig` (used to construct a client when no direct
    /// client is injected).
    sdk_config: Option<aws_config::SdkConfig>,
    /// Optional region override; ignored if a client was injected directly.
    region: Option<Region>,
    /// Optional capabilities override (wins over the `caps_for` lookup).
    capabilities_override: Option<ModelCapabilities>,
    /// Optional `max_output_tokens` override.
    max_output_override: Option<u32>,
}

impl BedrockModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            client: None,
            sdk_config: None,
            region: None,
            capabilities_override: None,
            max_output_override: None,
        }
    }

    /// Inject a pre-constructed Bedrock SDK client.
    ///
    /// This takes highest precedence.  When a client is injected any
    /// `.region(…)` call is **ignored** (the client already has a region baked
    /// in) and `.sdk_config(…)` is not used to construct the client either.
    pub fn client(mut self, c: Client) -> Self {
        self.client = Some(c);
        self
    }

    /// Build the SDK client from a caller-provided [`aws_config::SdkConfig`].
    ///
    /// Ignored if a direct client was already injected via `.client(…)`.
    pub fn sdk_config(mut self, c: &aws_config::SdkConfig) -> Self {
        self.sdk_config = Some(c.clone());
        self
    }

    /// Override the AWS region.
    ///
    /// **Note:** this setting is **ignored** when a client is injected via
    /// `.client(…)` because the client already has its region baked in.  The
    /// region is only applied when building a client from `.sdk_config(…)` or
    /// when using [`crate::BedrockModel::from_env`].
    pub fn region(mut self, r: impl Into<Region>) -> Self {
        self.region = Some(r.into());
        self
    }

    /// Override the capability flags for this model instance.
    ///
    /// Wins over the built-in `caps_for` lookup table.
    pub fn capabilities(mut self, c: ModelCapabilities) -> Self {
        self.capabilities_override = Some(c);
        self
    }

    /// Override the default `max_output_tokens` passed to Bedrock when the
    /// caller does not set `ModelSettings::max_output_tokens`.
    pub fn max_output_tokens_default(mut self, n: u32) -> Self {
        self.max_output_override = Some(n);
        self
    }

    /// Resolve all settings and construct the [`crate::BedrockModel`].
    ///
    /// This is **synchronous** — no network calls are made.
    ///
    /// # Errors
    ///
    /// - [`BuildError::EmptyModelId`] — model id is an empty string.
    /// - [`BuildError::MissingClient`] — neither `.client(…)` nor
    ///   `.sdk_config(…)` was called (use [`crate::BedrockModel::from_env`] for
    ///   automatic env-based client construction).
    pub fn build(self) -> Result<crate::BedrockModel, BuildError> {
        // 1. Validate model id.
        if self.model_id.is_empty() {
            return Err(BuildError::EmptyModelId);
        }

        // 2. Resolve client.
        let client = if let Some(c) = self.client {
            // Injected client wins; region override is ignored.
            if self.region.is_some() {
                tracing::debug!(
                    target: "paigasus::bedrock::builder",
                    "region() was set but a Client was injected — region override ignored; \
                     the injected client's region takes precedence",
                );
            }
            c
        } else if let Some(ref sdk_cfg) = self.sdk_config {
            Client::new(sdk_cfg)
        } else {
            return Err(BuildError::MissingClient);
        };

        // 3. Detect family and resolve capabilities.
        let family = ModelFamily::from_model_id(&self.model_id);
        let default_caps = caps_for(family);
        let capabilities = self.capabilities_override.unwrap_or(default_caps);
        let max_output_default = self.max_output_override;

        Ok(crate::BedrockModel(Arc::new(Config {
            client,
            model_id: self.model_id,
            family,
            capabilities,
            max_output_default,
        })))
    }
}

// ── BedrockModel constructors (here because they construct a builder) ─────────

impl crate::BedrockModel {
    /// Start building a Bedrock model for the given model id.
    ///
    /// ```ignore
    /// use paigasus_helikon_providers_bedrock::BedrockModel;
    /// # async fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let model = BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
    ///     .sdk_config(&aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
    ///         .load().await)
    ///     .build()?;
    /// # Ok(()) }
    /// ```
    pub fn converse(model_id: impl Into<String>) -> BedrockModelBuilder {
        BedrockModelBuilder::new(model_id)
    }

    /// Construct a [`crate::BedrockModel`] by loading AWS configuration from
    /// the environment (credential files, environment variables, IMDS, SSO…).
    ///
    /// ## Credential laziness
    ///
    /// The AWS credential chain is **lazy** — loading the config does not prove
    /// that your credentials will be accepted by Bedrock.  Auth/permission
    /// failures surface at invoke time as a
    /// [`paigasus_helikon_core::ModelError`].
    ///
    /// ## BehaviorVersion
    ///
    /// Uses the pinned `BehaviorVersion::v2026_01_12()` so that a Dependabot
    /// `aws-config` bump cannot silently shift SDK behavior.
    ///
    /// ## Region
    ///
    /// Pass `None` to use the region from the environment
    /// (`AWS_DEFAULT_REGION`, `~/.aws/config`, …).  Pass `Some(region)` to
    /// override it.
    pub async fn from_env(model_id: impl Into<String>) -> Result<Self, BuildError> {
        Self::from_env_with_region(model_id, None::<Region>).await
    }

    /// Like [`from_env`][Self::from_env] but with an explicit region override.
    pub async fn from_env_with_region(
        model_id: impl Into<String>,
        region: Option<impl Into<Region>>,
    ) -> Result<Self, BuildError> {
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12());
        if let Some(r) = region {
            loader = loader.region(r.into());
        }
        let sdk_cfg = loader.load().await;
        BedrockModelBuilder::new(model_id)
            .sdk_config(&sdk_cfg)
            .build()
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use aws_config::BehaviorVersion;
    use paigasus_helikon_core::Model;

    /// Build an offline [`Client`] suitable for unit tests (no real AWS calls).
    fn offline_client() -> Client {
        // Use aws_config::ConfigLoader::test_credentials() to inject static
        // dummy credentials that satisfy the SDK's builder validation without
        // hitting any AWS endpoint.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            let sdk_cfg = aws_config::defaults(BehaviorVersion::v2026_01_12())
                .region(Region::new("us-east-1"))
                .test_credentials()
                .load()
                .await;
            Client::new(&sdk_cfg)
        })
    }

    #[test]
    fn empty_model_id_returns_err_empty_model_id() {
        let result = crate::BedrockModel::converse("")
            .client(offline_client())
            .build();
        assert!(
            matches!(result, Err(BuildError::EmptyModelId)),
            "expected EmptyModelId, got {result:?}",
        );
    }

    #[test]
    fn no_client_or_config_returns_err_missing_client() {
        let result =
            crate::BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0").build();
        assert!(
            matches!(result, Err(BuildError::MissingClient)),
            "expected MissingClient, got {result:?}",
        );
    }

    #[test]
    fn injecting_client_succeeds() {
        let model = crate::BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
            .client(offline_client())
            .build()
            .expect("should build with injected client");
        assert_eq!(model.model(), "anthropic.claude-3-5-sonnet-20241022-v2:0");
    }

    #[test]
    fn sdk_config_path_succeeds() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12())
                .region(Region::new("eu-west-1"))
                .test_credentials()
                .load()
                .await;
            let model = crate::BedrockModel::converse("amazon.nova-pro-v1:0")
                .sdk_config(&sdk_cfg)
                .build()
                .expect("should build from sdk_config");
            assert_eq!(model.model(), "amazon.nova-pro-v1:0");
        });
    }

    #[test]
    fn capabilities_come_from_caps_for_by_default() {
        let model = crate::BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
            .client(offline_client())
            .build()
            .expect("build");
        let caps = model.capabilities();
        assert!(caps.streaming, "anthropic should report streaming");
        assert!(caps.tools, "anthropic should report tools");
        assert!(
            caps.structured_output,
            "anthropic should report structured_output"
        );
    }

    #[test]
    fn capabilities_override_wins_over_lookup() {
        let custom = ModelCapabilities::empty();
        let model = crate::BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
            .client(offline_client())
            .capabilities(custom)
            .build()
            .expect("build");
        let caps = model.capabilities();
        assert!(!caps.tools, "override should clear tools");
        assert!(
            !caps.structured_output,
            "override should clear structured_output"
        );
    }

    #[test]
    fn max_output_override_is_stored() {
        let model = crate::BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
            .client(offline_client())
            .max_output_tokens_default(1234)
            .build()
            .expect("build");
        assert_eq!(model.0.max_output_default, Some(1234));
    }

    #[test]
    fn max_output_default_is_none_when_not_set() {
        let model = crate::BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
            .client(offline_client())
            .build()
            .expect("build");
        assert_eq!(model.0.max_output_default, None);
    }

    #[test]
    fn region_ignored_when_client_injected() {
        // Should succeed without warning-as-error: the tracing::debug! inside
        // build() does NOT panic. We just verify build succeeds.
        let model = crate::BedrockModel::converse("anthropic.claude-3-5-sonnet-20241022-v2:0")
            .client(offline_client())
            .region(Region::new("ap-southeast-1"))
            .build()
            .expect("build should succeed even when region is set with injected client");
        assert_eq!(model.model(), "anthropic.claude-3-5-sonnet-20241022-v2:0");
    }

    #[test]
    fn llama_family_has_no_structured_output() {
        let model = crate::BedrockModel::converse("meta.llama3-1-70b-instruct-v1:0")
            .client(offline_client())
            .build()
            .expect("build");
        assert!(!model.capabilities().structured_output);
    }
}

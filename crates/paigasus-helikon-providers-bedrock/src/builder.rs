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
use crate::policy::{ForcedChoicePolicy, OutputPath};

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
    /// as `"global.anthropic.claude-sonnet-5-5"`.
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
    /// Bedrock model identifier (e.g. `global.anthropic.claude-sonnet-5-5`).
    pub(crate) model_id: String,
    /// Effective model family: the builder override, else the family detected from the model id.
    pub(crate) family: ModelFamily,
    /// Resolved capability flags for this model instance.
    pub(crate) capabilities: ModelCapabilities,
    /// Optional default for `max_tokens` when the caller does not set it.
    ///
    /// When `None` (the default), `inferenceConfig.maxTokens` is omitted from
    /// the Bedrock request so that each model applies its own correct limit.
    /// Set via [`BedrockModelBuilder::max_output_tokens_default`].
    pub(crate) max_output_default: Option<u32>,
    /// Send the typed `memory_*` tool entry for a tool that declares one.
    /// Set via [`BedrockModelBuilder::native_tool_types`].
    pub(crate) native_tool_types: bool,
}

impl Config {
    /// `true` when the request may carry a typed tool entry.
    ///
    /// The gate needs `native_tool_types` and an Anthropic family. This is the
    /// only place that decides it.
    pub(crate) fn sends_typed_tools(&self) -> bool {
        self.native_tool_types && self.family == ModelFamily::Anthropic
    }
}

/// Build an offline [`Client`] for unit tests. The client makes no AWS call.
///
/// This function starts its own Tokio runtime, so do not call it from inside
/// a runtime.
#[cfg(test)]
pub(crate) fn offline_client() -> Client {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_11_17())
            .region(Region::new("us-east-1"))
            .test_credentials()
            .load()
            .await;
        Client::new(&sdk_cfg)
    })
}

/// The effective family: the override when set, else the family from the model id.
pub(crate) fn resolve_family(
    detected: ModelFamily,
    family_override: Option<ModelFamily>,
) -> ModelFamily {
    family_override.unwrap_or(detected)
}

/// Make an offline [`Config`] for unit tests. The client makes no AWS call.
///
/// This function starts its own Tokio runtime, so do not call it from inside
/// a runtime.
#[cfg(test)]
pub(crate) fn offline_config(model_id: &str) -> Config {
    let family = resolve_family(ModelFamily::from_model_id(model_id), None);
    let capabilities = caps_for(family);
    Config {
        client: offline_client(),
        model_id: model_id.to_owned(),
        family,
        capabilities,
        max_output_default: None,
        native_tool_types: true,
    }
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
    /// Optional family override (wins over detection from the model id).
    family_override: Option<ModelFamily>,
    /// Send the typed `memory_*` tool entry (default `true`).
    native_tool_types: bool,
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
            family_override: None,
            native_tool_types: true,
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
    ///
    /// It changes only what `capabilities()` reports. It does not change the
    /// request. To turn on the family-gated request behavior for an opaque
    /// model ID, use [`BedrockModelBuilder::family`].
    pub fn capabilities(mut self, c: ModelCapabilities) -> Self {
        self.capabilities_override = Some(c);
        self
    }

    /// Set the model family and turn off detection from the model id.
    ///
    /// The family controls the request translation, the default capabilities
    /// and the forced-choice policy, like a detected family. The
    /// [`capabilities`](Self::capabilities) override still wins for the
    /// reported flags. A second call replaces the first.
    ///
    /// Use this for opaque model IDs: application inference profiles,
    /// provisioned-throughput ARNs and custom-model ARNs. These IDs do not
    /// name the family, so detection gives `ModelFamily::Unknown`.
    ///
    /// With `Anthropic`, `Mistral` or `AmazonNova`, the provider sends
    /// `ToolChoice::Required` and `ToolChoice::Tool` on the forced-tool path.
    /// Some requests then fail locally:
    ///
    /// - `ResponseFormat::JsonSchema` or `JsonObject` with `ToolChoice::Tool`.
    /// - `ResponseFormat::JsonSchema` or `JsonObject` with `ToolChoice::None`.
    /// - `ToolChoice::Required` with no tools and no `ResponseFormat::JsonSchema`
    ///   or `JsonObject`. Each of these formats adds a synthesized tool.
    /// - `ToolChoice::Tool` with a name that no tool defines.
    ///
    /// On the native path, the provider omits these choices and logs a
    /// warning. The native path applies to Claude Sonnet 5.5 and Opus 5.5,
    /// and to a handle after one rejection.
    ///
    /// A profile on Claude Sonnet 5.5 or Opus 5.5 costs one rejected request
    /// for each handle. Build one handle and clone it.
    ///
    /// ```ignore
    /// use paigasus_helikon_providers_bedrock::{BedrockModel, ModelFamily};
    /// # fn f(arn: &str, cfg: &aws_config::SdkConfig) -> Result<(), Box<dyn std::error::Error>> {
    /// let model = BedrockModel::converse(arn)
    ///     .sdk_config(cfg)
    ///     .family(ModelFamily::Anthropic)
    ///     .build()?;
    /// # Ok(()) }
    /// ```
    pub fn family(mut self, f: ModelFamily) -> Self {
        self.family_override = Some(f);
        self
    }

    /// Override the default `max_output_tokens` passed to Bedrock when the
    /// caller does not set `ModelSettings::max_output_tokens`.
    pub fn max_output_tokens_default(mut self, n: u32) -> Self {
        self.max_output_override = Some(n);
        self
    }

    /// Send the Anthropic typed tool entry for a tool that declares a
    /// provider tool type for the Anthropic provider. Default: `true`. Set
    /// `false` to always send the generic entry (name, description and input
    /// schema) and ignore declared provider types.
    ///
    /// Only Claude models are affected. Only `memory_*` types are affected.
    /// Other model families and other tool types keep the generic entry.
    pub fn native_tool_types(mut self, enabled: bool) -> Self {
        self.native_tool_types = enabled;
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
            if let Some(region) = self.region {
                // Region override: build a per-client config that layers the
                // override on top of the caller's SdkConfig, then construct
                // the client from that config.
                let client_cfg = aws_sdk_bedrockruntime::config::Builder::from(sdk_cfg)
                    .region(region)
                    .build();
                aws_sdk_bedrockruntime::Client::from_conf(client_cfg)
            } else {
                Client::new(sdk_cfg)
            }
        } else {
            return Err(BuildError::MissingClient);
        };

        // 3. Detect family and resolve capabilities.
        let detected = ModelFamily::from_model_id(&self.model_id);
        let family = resolve_family(detected, self.family_override);
        match self.family_override {
            None if family == ModelFamily::Unknown => {
                tracing::debug!(
                    target: "paigasus::bedrock::builder",
                    model_id = %self.model_id,
                    "model family not detected from the model id; structured-output synthesis and forced tool choice are off; set BedrockModelBuilder::family to choose the family",
                );
            }
            Some(f)
                if detected != ModelFamily::Unknown
                    && f != ModelFamily::Unknown
                    && detected != f =>
            {
                tracing::warn!(
                    target: "paigasus::bedrock::builder",
                    model_id = %self.model_id,
                    detected = ?detected,
                    family = ?f,
                    "the family override differs from the family that the model id names",
                );
            }
            Some(f) => {
                tracing::debug!(
                    target: "paigasus::bedrock::builder",
                    model_id = %self.model_id,
                    family = ?f,
                    "the model family comes from BedrockModelBuilder::family",
                );
            }
            None => {}
        }
        let default_caps = caps_for(family);
        let capabilities = self.capabilities_override.unwrap_or(default_caps);
        let max_output_default = self.max_output_override;

        let policy = ForcedChoicePolicy::new(&self.model_id, family);
        if policy.path() == OutputPath::Native {
            tracing::debug!(
                target: "paigasus::bedrock::builder",
                model_id = %self.model_id,
                "the static rule selects native structured output for this model",
            );
        }

        Ok(crate::BedrockModel {
            config: Arc::new(Config {
                client,
                model_id: self.model_id,
                family,
                capabilities,
                max_output_default,
                native_tool_types: self.native_tool_types,
            }),
            policy: Arc::new(policy),
        })
    }
}

// ── BedrockModel constructors (here because they construct a builder) ─────────

impl crate::BedrockModel {
    /// Start building a Bedrock model for the given model id.
    ///
    /// ```ignore
    /// use paigasus_helikon_providers_bedrock::BedrockModel;
    /// # async fn f() -> Result<(), Box<dyn std::error::Error>> {
    /// let model = BedrockModel::converse("global.anthropic.claude-sonnet-5-5")
    ///     .sdk_config(&aws_config::defaults(aws_config::BehaviorVersion::v2026_11_17())
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
    /// Uses the pinned `BehaviorVersion::v2026_11_17()` so that a Dependabot
    /// `aws-config` bump cannot silently shift SDK behavior. This version
    /// applies the AWS SDK retry behavior from the Retry Behavior 2.1 spec.
    /// It also applies a default read timeout to AWS SDK clients.
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
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::v2026_11_17());
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
    use paigasus_helikon_core::{
        ContentPart, Item, Model, ModelRequest, ResponseFormat, ToolChoice, ToolDef,
    };
    use serde_json::json;

    use crate::translate::tools::SYNTHESIZED_TOOL_NAME;
    use crate::translate::{build_request, to_wire_json};

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
        let result = crate::BedrockModel::converse("anthropic.claude-sonnet-4-6").build();
        assert!(
            matches!(result, Err(BuildError::MissingClient)),
            "expected MissingClient, got {result:?}",
        );
    }

    #[test]
    fn injecting_client_succeeds() {
        let model = crate::BedrockModel::converse("anthropic.claude-sonnet-4-6")
            .client(offline_client())
            .build()
            .expect("should build with injected client");
        assert_eq!(model.model(), "anthropic.claude-sonnet-4-6");
    }

    #[test]
    fn sdk_config_path_succeeds() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_11_17())
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
        let model = crate::BedrockModel::converse("anthropic.claude-sonnet-4-6")
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
        let model = crate::BedrockModel::converse("anthropic.claude-sonnet-4-6")
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
        let model = crate::BedrockModel::converse("anthropic.claude-sonnet-4-6")
            .client(offline_client())
            .max_output_tokens_default(1234)
            .build()
            .expect("build");
        assert_eq!(model.config.max_output_default, Some(1234));
    }

    #[test]
    fn max_output_default_is_none_when_not_set() {
        let model = crate::BedrockModel::converse("anthropic.claude-sonnet-4-6")
            .client(offline_client())
            .build()
            .expect("build");
        assert_eq!(model.config.max_output_default, None);
    }

    #[test]
    fn region_ignored_when_client_injected() {
        // Should succeed without warning-as-error: the tracing::debug! inside
        // build() does NOT panic. We just verify build succeeds.
        let model = crate::BedrockModel::converse("anthropic.claude-sonnet-4-6")
            .client(offline_client())
            .region(Region::new("ap-southeast-1"))
            .build()
            .expect("build should succeed even when region is set with injected client");
        assert_eq!(model.model(), "anthropic.claude-sonnet-4-6");
    }

    /// Passing `.sdk_config()` + `.region()` should succeed and cover the
    /// region-override code path (we cannot inspect the baked-in region on a
    /// `Client` without a live endpoint, but we verify `build()` is `Ok`).
    #[test]
    fn sdk_config_with_region_override_succeeds() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async {
            let sdk_cfg = aws_config::defaults(aws_config::BehaviorVersion::v2026_11_17())
                .region(Region::new("eu-west-1"))
                .test_credentials()
                .load()
                .await;
            // Override to a different region via .region() on the sdk_config path.
            let model = crate::BedrockModel::converse("amazon.nova-pro-v1:0")
                .sdk_config(&sdk_cfg)
                .region(Region::new("ap-southeast-1"))
                .build()
                .expect("should build with region override on sdk_config path");
            assert_eq!(model.model(), "amazon.nova-pro-v1:0");
        });
    }

    #[test]
    fn llama_family_has_no_structured_output() {
        let model = crate::BedrockModel::converse("meta.llama3-1-70b-instruct-v1:0")
            .client(offline_client())
            .build()
            .expect("build");
        assert!(!model.capabilities().structured_output);
    }

    #[test]
    fn global_claude_profile_has_structured_output() {
        let model = crate::BedrockModel::converse("global.anthropic.claude-sonnet-5-5")
            .client(offline_client())
            .build()
            .expect("build");
        assert!(model.capabilities().structured_output);
        assert!(model.capabilities().parallel_tool_calls);
    }

    const ARN: &str =
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3d4e5f6";
    const SONNET55: &str = "global.anthropic.claude-sonnet-5-5";

    fn user_text(text: &str) -> Item {
        Item::UserMessage {
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
        }
    }

    fn json_schema_request() -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("Coffee, 4.50 EUR")];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Transaction".to_owned(),
            schema: json!({"type": "object", "properties": {"amount": {"type": "number"}}}),
            strict: false,
        });
        req
    }

    fn wire(model: &crate::BedrockModel, req: &ModelRequest) -> serde_json::Value {
        to_wire_json(&build_request(&model.config, req, model.policy.path()).unwrap())
    }

    fn assert_one_family(model: &crate::BedrockModel, want: ModelFamily) {
        assert_eq!(model.config.family, want);
        assert_eq!(model.policy.family(), want);
        assert_eq!(
            model.capabilities().structured_output,
            caps_for(want).structured_output
        );
    }

    fn build_with(id: &str, f: Option<ModelFamily>) -> crate::BedrockModel {
        let mut b = crate::BedrockModel::converse(id).client(offline_client());
        if let Some(f) = f {
            b = b.family(f);
        }
        b.build().expect("build")
    }

    #[test]
    fn family_override_turns_on_synthesis_for_opaque_arn() {
        let model = build_with(ARN, Some(ModelFamily::Anthropic));
        assert_one_family(&model, ModelFamily::Anthropic);
        let w = wire(&model, &json_schema_request());
        assert_eq!(
            w["toolConfig"]["toolChoice"],
            json!({"tool": {"name": SYNTHESIZED_TOOL_NAME}})
        );
        let tools = w["toolConfig"]["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .any(|t| t["toolSpec"]["name"] == SYNTHESIZED_TOOL_NAME));
        assert!(w.get("outputConfig").is_none());
    }

    #[test]
    fn opaque_arn_without_override_is_unchanged() {
        let model = build_with(ARN, None);
        assert_one_family(&model, ModelFamily::Unknown);
        let w = wire(&model, &json_schema_request());
        assert!(w["toolConfig"].is_null());
        assert!(w.get("outputConfig").is_none());
    }

    #[test]
    fn family_override_maps_required_tool_choice_to_any() {
        let model = build_with(ARN, Some(ModelFamily::Anthropic));
        assert_one_family(&model, ModelFamily::Anthropic);
        let mut req = ModelRequest::new();
        req.messages = vec![user_text("hi")];
        req.tools = vec![ToolDef::new("ping", "Ping", json!({"type": "object"}))];
        req.model_settings.tool_choice = Some(ToolChoice::Required);
        let w = wire(&model, &req);
        assert_eq!(w["toolConfig"]["toolChoice"], json!({"any": {}}));
    }

    #[test]
    fn capabilities_override_still_wins_over_family_override() {
        let model = crate::BedrockModel::converse(ARN)
            .client(offline_client())
            .capabilities(ModelCapabilities::empty())
            .family(ModelFamily::Anthropic)
            .build()
            .expect("build");
        assert!(!model.capabilities().structured_output);
        assert_eq!(model.config.family, ModelFamily::Anthropic);
    }

    #[test]
    fn second_family_call_replaces_first() {
        let model = crate::BedrockModel::converse(ARN)
            .client(offline_client())
            .family(ModelFamily::Mistral)
            .family(ModelFamily::Anthropic)
            .build()
            .expect("build");
        assert_one_family(&model, ModelFamily::Anthropic);
    }

    #[test]
    fn unknown_override_turns_off_synthesis_on_anthropic_id() {
        let model = build_with("anthropic.claude-sonnet-4-6", Some(ModelFamily::Unknown));
        assert_one_family(&model, ModelFamily::Unknown);
        let w = wire(&model, &json_schema_request());
        assert!(w["toolConfig"].is_null());
        assert!(w.get("outputConfig").is_none());
    }

    #[test]
    fn unknown_override_on_sonnet55_sends_no_output_config() {
        let model = build_with(SONNET55, Some(ModelFamily::Unknown));
        assert_one_family(&model, ModelFamily::Unknown);
        assert_eq!(model.policy.path(), OutputPath::ForcedTool);
        let w = wire(&model, &json_schema_request());
        assert!(w["toolConfig"].is_null());
        assert!(w.get("outputConfig").is_none());
        assert!(!model.capabilities().structured_output);
    }

    #[test]
    fn anthropic_override_on_sonnet55_keeps_native_path() {
        let model = build_with(SONNET55, Some(ModelFamily::Anthropic));
        assert_one_family(&model, ModelFamily::Anthropic);
        assert_eq!(model.policy.path(), OutputPath::Native);
        let w = wire(&model, &json_schema_request());
        assert!(w.get("outputConfig").is_some());
    }

    #[test]
    fn mistral_override_on_sonnet55_selects_native_path() {
        let model = build_with(SONNET55, Some(ModelFamily::Mistral));
        assert_one_family(&model, ModelFamily::Mistral);
        assert_eq!(model.policy.path(), OutputPath::Native);
        let w = wire(&model, &json_schema_request());
        assert!(w.get("outputConfig").is_some());
    }

    const CLAUDE: &str = "anthropic.claude-sonnet-4-6";

    #[test]
    fn native_tool_types_defaults_to_true() {
        let model = build_with(CLAUDE, None);
        assert!(model.config.native_tool_types);
    }

    #[test]
    fn native_tool_types_false_closes_the_gate() {
        let model = crate::BedrockModel::converse(CLAUDE)
            .client(offline_client())
            .native_tool_types(false)
            .build()
            .expect("build");
        assert!(!model.config.native_tool_types);
        assert!(!model.config.sends_typed_tools());
    }

    #[test]
    fn gate_is_open_only_for_anthropic() {
        assert!(build_with(CLAUDE, None).config.sends_typed_tools());
        assert!(!build_with("us.amazon.nova-pro-v1:0", None)
            .config
            .sends_typed_tools());
    }

    #[test]
    fn family_override_opens_the_gate() {
        let arn = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/abc123";
        assert!(!build_with(arn, None).config.sends_typed_tools());
        assert!(build_with(arn, Some(ModelFamily::Anthropic))
            .config
            .sends_typed_tools());
    }

    #[test]
    fn resolve_family_prefers_override() {
        let unknown = ModelFamily::Unknown;
        assert_eq!(resolve_family(unknown, None), ModelFamily::Unknown);
        assert_eq!(
            resolve_family(unknown, Some(ModelFamily::Anthropic)),
            ModelFamily::Anthropic
        );
        assert_eq!(
            resolve_family(ModelFamily::Anthropic, Some(ModelFamily::Unknown)),
            ModelFamily::Unknown
        );
    }
}

//! Typestate builder for [`crate::LlmAgent`]. See [SMA-319 design] for
//! the full rationale.
//!
//! [SMA-319 design]: https://github.com/SMK1085/paigasus-helikon/blob/main/docs/superpowers/specs/2026-05-28-sma-319-typestate-builder-design.md

/// Typestate marker: `.name(…)` has not been called yet.
pub struct NoName;

/// Typestate marker: `.name(…)` has been called; `.build()` is now reachable
/// once `HasModel` is also satisfied.
pub struct HasName;

/// Typestate marker: `.model(…)` / `.shared_model(…)` has not been called yet.
pub struct NoModel;

/// Typestate marker: `.model(…)` / `.shared_model(…)` has been called; `.build()`
/// is now reachable once `HasName` is also satisfied.
pub struct HasModel;

/// Typestate-driven builder for [`crate::LlmAgent`].
///
/// Constructed via [`crate::LlmAgent::builder()`]. `Ctx` is the per-run
/// context type; `M` is the concrete [`crate::Model`] implementation
/// (inferred from `.model(m)`); `T` is the structured-output type
/// (defaults to `String`; switched by `.output_type::<T>()`); `N` and
/// `Mo` are the typestate markers tracking which required setters have
/// been called.
///
/// `.build()` only exists once both `N = HasName` and `Mo = HasModel`.
/// Trying to `.build()` earlier is a compile error.
pub struct LlmAgentBuilder<Ctx, M, T, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    name: Option<String>,
    description: Option<String>,
    instructions: Option<std::sync::Arc<dyn crate::Instructions<Ctx>>>,
    model: Option<std::sync::Arc<M>>,
    tools: Vec<std::sync::Arc<dyn crate::Tool<Ctx>>>,
    handoffs: Vec<std::sync::Arc<dyn crate::Agent<Ctx>>>,
    output_type: Option<crate::OutputType>,
    input_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    output_guardrails: Vec<std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    hooks: Vec<std::sync::Arc<dyn crate::Hook<Ctx>>>,
    model_settings: crate::ModelSettings,
    config: crate::RunConfig,
    // `fn() -> _` (not `(N, Mo, T)`) so the builder is `Send + Sync` regardless
    // of N/Mo/T's auto-traits, and to keep typestate markers out of drop-check.
    #[allow(clippy::type_complexity)]
    _state: std::marker::PhantomData<fn() -> (N, Mo, T)>,
}

impl<Ctx> LlmAgentBuilder<Ctx, (), String, NoName, NoModel>
where
    Ctx: Send + Sync + 'static,
{
    /// Internal initial-state constructor. Called by
    /// [`crate::LlmAgent::builder()`]; not part of the public API
    /// (the double underscore is a "don't call from outside the
    /// crate" signal even though the method is `pub` for cross-module
    /// access).
    #[doc(hidden)]
    pub fn __new() -> Self {
        Self {
            name: None,
            description: None,
            instructions: None,
            model: None,
            tools: Vec::new(),
            handoffs: Vec::new(),
            output_type: None,
            input_guardrails: Vec::new(),
            output_guardrails: Vec::new(),
            hooks: Vec::new(),
            model_settings: crate::ModelSettings::default(),
            config: crate::RunConfig::default(),
            _state: std::marker::PhantomData,
        }
    }
}

// Any-state setters: callable in every typestate combination, return Self
// unchanged in (N, Mo, T) generics. Each takes `mut self`, mutates a field,
// returns Self.
impl<Ctx, M, T, N, Mo> LlmAgentBuilder<Ctx, M, T, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the agent's human-readable description.
    ///
    /// Used by handoff targets when their parent agent's prompt is being
    /// rendered. Defaults to `""` if unset; setting it improves multi-agent
    /// routing quality.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }

    /// Set the agent's system-prompt renderer.
    ///
    /// `Instructions` is implemented for `String`, `&'static str`, and any
    /// `Fn(&RunContext<Ctx>) -> String + Send + Sync`. The value is wrapped
    /// in an `Arc` internally — use [`Self::shared_instructions`] if you
    /// already hold an `Arc<dyn Instructions<Ctx>>`.
    pub fn instructions(mut self, i: impl crate::Instructions<Ctx> + 'static) -> Self {
        self.instructions = Some(std::sync::Arc::new(i));
        self
    }

    /// Set the agent's system-prompt renderer from a pre-wrapped trait object.
    ///
    /// Use this when the same `Instructions` impl is shared across multiple
    /// agents — avoids re-wrapping in another `Arc`.
    pub fn shared_instructions(mut self, i: std::sync::Arc<dyn crate::Instructions<Ctx>>) -> Self {
        self.instructions = Some(i);
        self
    }

    /// Append a tool to the agent's tool registry.
    ///
    /// Takes an owned value; wraps in `Arc` internally. Use
    /// [`Self::shared_tool`] for pre-wrapped trait objects.
    pub fn tool(mut self, t: impl crate::Tool<Ctx> + 'static) -> Self {
        self.tools
            .push(std::sync::Arc::new(t) as std::sync::Arc<dyn crate::Tool<Ctx>>);
        self
    }

    /// Append a pre-wrapped tool to the agent's tool registry.
    pub fn shared_tool(mut self, t: std::sync::Arc<dyn crate::Tool<Ctx>>) -> Self {
        self.tools.push(t);
        self
    }

    /// Replace the agent's tool registry with the supplied iterable.
    ///
    /// Accepts `Vec<Arc<dyn Tool<Ctx>>>`, the SMA-315 `tools![…]` macro
    /// output, or any other `IntoIterator`.
    pub fn tools<I>(mut self, t: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Tool<Ctx>>>,
    {
        self.tools = t.into_iter().collect();
        self
    }

    /// Append a handoff candidate.
    pub fn handoff(mut self, h: impl crate::Agent<Ctx> + 'static) -> Self {
        self.handoffs
            .push(std::sync::Arc::new(h) as std::sync::Arc<dyn crate::Agent<Ctx>>);
        self
    }

    /// Append a pre-wrapped handoff candidate.
    pub fn shared_handoff(mut self, h: std::sync::Arc<dyn crate::Agent<Ctx>>) -> Self {
        self.handoffs.push(h);
        self
    }

    /// Replace the handoff candidate list.
    pub fn handoffs<I>(mut self, h: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Agent<Ctx>>>,
    {
        self.handoffs = h.into_iter().collect();
        self
    }

    /// Append a lifecycle hook.
    pub fn hook(mut self, h: impl crate::Hook<Ctx> + 'static) -> Self {
        self.hooks
            .push(std::sync::Arc::new(h) as std::sync::Arc<dyn crate::Hook<Ctx>>);
        self
    }

    /// Append a pre-wrapped lifecycle hook.
    pub fn shared_hook(mut self, h: std::sync::Arc<dyn crate::Hook<Ctx>>) -> Self {
        self.hooks.push(h);
        self
    }

    /// Replace the hook list.
    pub fn hooks<I>(mut self, h: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Hook<Ctx>>>,
    {
        self.hooks = h.into_iter().collect();
        self
    }

    /// Append an input guardrail.
    pub fn input_guardrail(mut self, g: impl crate::Guardrail<Ctx> + 'static) -> Self {
        self.input_guardrails
            .push(std::sync::Arc::new(g) as std::sync::Arc<dyn crate::Guardrail<Ctx>>);
        self
    }

    /// Append a pre-wrapped input guardrail.
    pub fn shared_input_guardrail(mut self, g: std::sync::Arc<dyn crate::Guardrail<Ctx>>) -> Self {
        self.input_guardrails.push(g);
        self
    }

    /// Replace the input-guardrail list.
    pub fn input_guardrails<I>(mut self, g: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    {
        self.input_guardrails = g.into_iter().collect();
        self
    }

    /// Append an output guardrail.
    pub fn output_guardrail(mut self, g: impl crate::Guardrail<Ctx> + 'static) -> Self {
        self.output_guardrails
            .push(std::sync::Arc::new(g) as std::sync::Arc<dyn crate::Guardrail<Ctx>>);
        self
    }

    /// Append a pre-wrapped output guardrail.
    pub fn shared_output_guardrail(mut self, g: std::sync::Arc<dyn crate::Guardrail<Ctx>>) -> Self {
        self.output_guardrails.push(g);
        self
    }

    /// Replace the output-guardrail list.
    pub fn output_guardrails<I>(mut self, g: I) -> Self
    where
        I: IntoIterator<Item = std::sync::Arc<dyn crate::Guardrail<Ctx>>>,
    {
        self.output_guardrails = g.into_iter().collect();
        self
    }

    /// Replace the [`crate::ModelSettings`] applied to every model call.
    pub fn model_settings(mut self, s: crate::ModelSettings) -> Self {
        self.model_settings = s;
        self
    }

    /// Set the per-run `max_turns` budget.
    ///
    /// Equivalent to constructing a [`crate::RunConfig`] with the specified
    /// `max_turns` and passing it via `.config(…)` (SMA-321 will add the
    /// full `.config` setter).
    pub fn max_turns(mut self, n: u32) -> Self {
        self.config.max_turns = n;
        self
    }
}

// .name(…) — only callable when the Name marker is NoName. Transitions
// to HasName, leaving every other generic parameter unchanged.
impl<Ctx, M, T, Mo> LlmAgentBuilder<Ctx, M, T, NoName, Mo>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the agent's name and transition the typestate to `HasName`.
    ///
    /// Once called, `.name` is no longer in scope — calling it a second
    /// time is a compile error.
    pub fn name(self, n: impl Into<String>) -> LlmAgentBuilder<Ctx, M, T, HasName, Mo> {
        LlmAgentBuilder {
            name: Some(n.into()),
            description: self.description,
            instructions: self.instructions,
            model: self.model,
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: self.output_type,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _state: std::marker::PhantomData,
        }
    }
}

// .model(…) / .shared_model(…) — only callable when the Model marker is
// NoModel. Transition consumes self and rebuilds with the new M2 generic
// inferred from the model argument.
impl<Ctx, M0, T, N> LlmAgentBuilder<Ctx, M0, T, N, NoModel>
where
    Ctx: Send + Sync + 'static,
{
    /// Set the agent's model from an owned value.
    ///
    /// `M2` is inferred from the argument type; the builder transitions
    /// to `LlmAgentBuilder<Ctx, M2, T, N, HasModel>`. Wraps the value in
    /// an `Arc` internally — use [`Self::shared_model`] if the model is
    /// already shared across multiple agents.
    pub fn model<M2>(self, m: M2) -> LlmAgentBuilder<Ctx, M2, T, N, HasModel>
    where
        M2: crate::Model + 'static,
    {
        self.shared_model(std::sync::Arc::new(m))
    }

    /// Set the agent's model from a pre-wrapped `Arc`.
    ///
    /// Stores the supplied `Arc` directly — no re-wrapping.
    pub fn shared_model<M2>(self, m: std::sync::Arc<M2>) -> LlmAgentBuilder<Ctx, M2, T, N, HasModel>
    where
        M2: crate::Model + 'static,
    {
        LlmAgentBuilder {
            name: self.name,
            description: self.description,
            instructions: self.instructions,
            model: Some(m),
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: self.output_type,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _state: std::marker::PhantomData,
        }
    }
}

// .build() — only available on the fully-constructed state. The typestate
// guarantees `.name` and `.model` were both called, so the corresponding
// `Option`s are `Some`. We `.expect` with typestate-referencing messages
// for diagnostic clarity if the unreachable ever fires.
impl<Ctx, M, T> LlmAgentBuilder<Ctx, M, T, HasName, HasModel>
where
    Ctx: Send + Sync + 'static,
    M: crate::Model + 'static,
    T: Send + Sync + 'static,
{
    /// Finalize the builder into an [`crate::LlmAgent`].
    ///
    /// Only available when the builder has transitioned to both
    /// `HasName` and `HasModel`. Earlier states do not have a `.build`
    /// method in scope — `cargo build` fails with a clear error.
    pub fn build(self) -> crate::LlmAgent<Ctx, M, T> {
        crate::LlmAgent {
            name: self.name.expect("typestate HasName guarantees Some"),
            description: self.description.unwrap_or_default(),
            instructions: self
                .instructions
                .unwrap_or_else(|| std::sync::Arc::new(String::new())),
            model: self.model.expect("typestate HasModel guarantees Some"),
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: self.output_type,
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _output: std::marker::PhantomData,
        }
    }
}

// .output_type::<T>() — any-state, repeatable. Each call is a typestate
// transition that swaps the T generic and populates the OutputType schema.
impl<Ctx, M, T0, N, Mo> LlmAgentBuilder<Ctx, M, T0, N, Mo>
where
    Ctx: Send + Sync + 'static,
{
    /// Switch the structured-output type to `T2`.
    ///
    /// `T2 = String` (the default) is a no-op semantically (the
    /// `output_type` field becomes `Some(schema_for_string)`, which the
    /// runner treats the same as the default); pass any other `T2` to
    /// configure structured output. The runtime wiring lands in SMA-320.
    ///
    /// `DeserializeOwned` is required by SMA-320's runtime path
    /// (deserializing the model's response into `T2`); pinned here so
    /// the bound doesn't tighten under callers when SMA-320 lands.
    /// `Send + Sync + 'static` is needed by `Agent::run`'s async boundary
    /// (and also enforced at `.build()` — duplicating it here surfaces
    /// the error at the call site that picked the wrong `T`).
    pub fn output_type<T2>(self) -> LlmAgentBuilder<Ctx, M, T2, N, Mo>
    where
        T2: Send + Sync + 'static + serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        LlmAgentBuilder {
            name: self.name,
            description: self.description,
            instructions: self.instructions,
            model: self.model,
            tools: self.tools,
            handoffs: self.handoffs,
            output_type: Some(crate::OutputType::from_schema::<T2>()),
            input_guardrails: self.input_guardrails,
            output_guardrails: self.output_guardrails,
            hooks: self.hooks,
            model_settings: self.model_settings,
            config: self.config,
            _state: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        CancellationToken, Instructions, LlmAgent, Model, ModelCapabilities, ModelError,
        ModelEvent, ModelRequest, Tool, ToolContext, ToolError, ToolOutput,
    };
    use async_trait::async_trait;
    use futures_core::stream::BoxStream;
    use std::sync::Arc;

    // ── Tiny stubs that exist solely to compile against the typestate API.
    // The trybuild fixtures cover the *typestate* error surface; these unit
    // tests cover the *behavioral* surface (field plumbing, defaults).

    struct StubModel;
    #[async_trait]
    impl Model for StubModel {
        async fn invoke(
            &self,
            _r: ModelRequest,
            _c: CancellationToken,
        ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
            Err(ModelError::Unavailable)
        }
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::default()
        }
    }

    struct StubTool;
    #[async_trait]
    impl<Ctx> Tool<Ctx> for StubTool
    where
        Ctx: Send + Sync + 'static,
    {
        fn name(&self) -> &str {
            "stub"
        }
        fn description(&self) -> &str {
            "stub tool"
        }
        fn schema(&self) -> &serde_json::Value {
            static S: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
            S.get_or_init(|| serde_json::json!({"type":"object"}))
        }
        async fn invoke(
            &self,
            _c: &ToolContext<Ctx>,
            _a: serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                content: serde_json::Value::String("ok".into()),
            })
        }
    }

    #[test]
    fn description_set_via_builder() {
        let agent = LlmAgent::builder::<()>()
            .description("triage agent")
            .name("triage")
            .model(StubModel)
            .build();
        assert_eq!(agent.description, "triage agent");
    }

    #[test]
    fn name_transitions_to_has_name() {
        // If this compiles, the transition typestate is correctly wired.
        // The downstream `.build()` requires HasName + HasModel, so we
        // chain `.model(…).build()` to prove the resulting builder is
        // in the right state.
        let agent = LlmAgent::builder::<()>()
            .name("triage")
            .model(StubModel)
            .build();
        assert_eq!(agent.name, "triage");
    }

    #[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
    struct Answer {
        value: u32,
    }

    #[derive(Debug, Default, PartialEq, serde::Deserialize, schemars::JsonSchema)]
    struct Score {
        points: u32,
    }

    #[test]
    fn output_type_populates_schema() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .output_type::<Answer>()
            .build();
        let expected = serde_json::to_value(schemars::schema_for!(Answer)).unwrap();
        let actual = serde_json::to_value(&agent.output_type.unwrap().schema).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn output_type_last_call_wins() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .output_type::<Answer>()
            .output_type::<Score>()
            .build();
        let expected = serde_json::to_value(schemars::schema_for!(Score)).unwrap();
        let actual = serde_json::to_value(&agent.output_type.unwrap().schema).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn build_with_required_only_uses_defaults() {
        let agent = LlmAgent::builder::<()>().name("a").model(StubModel).build();
        assert_eq!(agent.description, "");
        assert!(agent.tools.is_empty());
        assert!(agent.handoffs.is_empty());
        assert!(agent.hooks.is_empty());
        assert!(agent.input_guardrails.is_empty());
        assert!(agent.output_guardrails.is_empty());
        assert!(agent.output_type.is_none());
        assert_eq!(agent.config.max_turns, 16);
    }

    #[test]
    fn singular_tool_adders_append() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .tool(StubTool)
            .tool(StubTool)
            .build();
        assert_eq!(agent.tools.len(), 2);
    }

    #[test]
    fn plural_tools_setter_replaces() {
        let pre: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(StubTool)];
        let post: Vec<Arc<dyn Tool<()>>> = vec![Arc::new(StubTool), Arc::new(StubTool)];
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .tools(pre)
            .tools(post) // second call replaces
            .build();
        assert_eq!(agent.tools.len(), 2);
    }

    #[test]
    fn shared_tool_does_not_double_wrap() {
        let shared: Arc<dyn Tool<()>> = Arc::new(StubTool);
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .shared_tool(Arc::clone(&shared))
            .build();
        assert_eq!(agent.tools.len(), 1);
        assert!(Arc::ptr_eq(&agent.tools[0], &shared));
    }

    #[test]
    fn shared_model_does_not_double_wrap() {
        let shared: Arc<StubModel> = Arc::new(StubModel);
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .shared_model(Arc::clone(&shared))
            .build();
        assert!(Arc::ptr_eq(&agent.model, &shared));
    }

    #[test]
    fn shared_instructions_does_not_double_wrap() {
        let shared: Arc<dyn Instructions<()>> = Arc::new(String::from("you are helpful"));
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .shared_instructions(Arc::clone(&shared))
            .build();
        assert!(Arc::ptr_eq(&agent.instructions, &shared));
    }

    #[test]
    fn max_turns_overrides_default() {
        let agent = LlmAgent::builder::<()>()
            .name("a")
            .model(StubModel)
            .max_turns(99)
            .build();
        assert_eq!(agent.config.max_turns, 99);
    }
}

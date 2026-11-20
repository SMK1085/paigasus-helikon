# SMA-830: let a tool declare a provider tool type

Linear: [SMA-830](https://linear.app/smaschek/issue/SMA-830/let-a-tool-declare-a-provider-tool-type-native-anthropic-memory).
Consumer: SMA-789 (the agent host's `memory` tool).

## Intent

The agent host (SMA-789) ships a custom `memory` tool. That tool copies the
commands, argument names and return strings of Anthropic's `memory_20250818`
tool, and it puts the protocol text in its description. Claude is trained on
the native tool, and the API adds the memory protocol to the system prompt only
when the request contains the native entry.

This change lets a Helikon tool declare a provider tool type. The Anthropic
provider then sends the native entry:

```json
{"type": "memory_20250818", "name": "memory"}
```

That entry has no `description` and no `input_schema`. All other providers
continue to send the generic schema. The host can change to the native type
with no change to its schema.

## Goals

- A `Tool` can declare zero or more provider tool types.
- Each declaration names a provider, a type string, and optional extra JSON
  fields. The extra fields cover tools such as `text_editor_20250728`
  (`max_characters`) and `computer_*` (display size). The limit: a tool has at
  most one type per provider, and the provider sends that type to every model.
  A tool type that depends on the model generation (for example the
  `text_editor` versions) needs the opt-out below.
- The Anthropic provider sends the typed entry when the tool declares an
  `anthropic` type. It sends the generic entry when the tool declares none.
- The OpenAI (Chat and Responses), Gemini, Bedrock and LiteLLM providers ignore
  the declaration and send the generic schema.
- Each provider has a test for both paths.

## Non-goals

- Native support on Bedrock. The Anthropic docs list the memory tool as
  supported on Amazon Bedrock, but our Bedrock provider uses the Converse API.
  A follow-up Linear issue covers it. LiteLLM can also route to Claude, but it
  uses the OpenAI wire format, so it also sends the generic schema.
- A `#[tool]` macro attribute for provider types. The SMA-789 host implements
  `Tool` by hand. A macro attribute can follow if a user asks for it.
- Server-side Anthropic tools (for example `web_search_*`). Those tools run on
  Anthropic's side and return no `tool_use` for Helikon to run. They need a
  different design.
- A model-dependent choice of type. The declaration is static.
  `AnthropicModelBuilder::native_tool_types(false)` is the opt-out for a model
  or gateway that does not know the type.
- Validation of the tool name against the type. For example, `memory_20250818`
  requires the name `memory`. The provider does not check this. The API
  returns a 400 error if the name is wrong.

## Anti-goals

- A change in behaviour for a tool that declares no provider type. The request
  bytes for such a tool must stay the same on every provider.
- A change in the serialized form of an existing `ToolDef`. Old serialized data
  must still deserialize, and a `ToolDef` with no provider types must serialize
  to the same JSON as today. The persisted carrier is the Temporal `call_model`
  activity input (`CallModelArgs.request: ModelRequest`,
  `runtime-temporal/src/activity_input.rs:262-268`), which goes into Temporal
  history. `AgentPlan` is not serialized.

## Approaches

Two decisions were open. Sven made both of them.

**Shape of the declaration.**

- A. A type string only. This is small, but it cannot express
  `text_editor` or `computer` parameters.
- **B. A type string plus optional extra JSON fields (chosen).** This covers
  every Anthropic client tool for a small increase in surface.
- C. A closed enum per known tool. This gives strong types, but each new tool
  version needs an SDK release.

**Carrier from `Tool` to the provider.**

- **1. A new field on `ToolDef`, and `ToolDef` becomes `#[non_exhaustive]`
  (chosen).** The declaration stays with the tool definition, and later fields
  are no longer breaking. Cost: a breaking change to `paigasus-helikon-core`
  and a release of all dependents.
- 2. A side map on `ModelRequest`, keyed by tool name. This is not breaking,
  but the data is not on the tool definition. Each provider must look it up by
  name, and code that builds a `ToolDef` by hand can forget the map.

**Rejected: a provider-side mapping**, for example
`AnthropicModelBuilder::native_tool("memory", ProviderToolType)`. It needs no
core change and no breaking release. But the declaration is then separate
from the tool, and a name-based mapping applies to every agent on that model.
The ticket asks for the tool to declare the type.

## Design

### Core (`paigasus-helikon-core`, `src/model.rs` and `src/tool.rs`)

A new public type:

```rust
/// A provider-defined tool type that a tool declares, for example
/// Anthropic's `memory_20250818`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct ProviderToolType {
    /// The provider id, for example `"anthropic"`.
    pub provider: String,
    /// The provider's type string, for example `"memory_20250818"`.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Extra fields that the provider puts into its tool entry.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
}
```

Each provider uses only the first entry with its own id. The `///` comment
says: declare at most one entry per provider.

```rust
impl ProviderToolType {
    pub fn new(provider: impl Into<String>, type_name: impl Into<String>) -> Self;
    pub fn with_params(self, params: serde_json::Map<String, serde_json::Value>) -> Self;
}
```

A new method on `Tool`, with a default:

```rust
/// Provider tool types that this tool declares. Default: none.
/// A tool that wraps another tool (a decorator) must forward this method.
/// If it does not, the declaration is lost.
fn provider_types(&self) -> &[ProviderToolType] { &[] }
```

`ToolDef` changes:

- It gets `pub provider_types: Vec<ProviderToolType>`, with
  `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
- It becomes `#[non_exhaustive]` and derives `PartialEq`. The precedent for
  this pattern is `ModelCapabilities` (`model.rs:267-299`).
- It gets constructors:
  - `ToolDef::new(name: impl Into<String>, description: impl Into<String>,
    schema: serde_json::Value) -> Self`, with empty `provider_types`;
  - `.with_provider_types(self, Vec<ProviderToolType>) -> Self`;
  - `ToolDef::from_tool<Ctx, T>(tool: &T) -> Self` where
    `Ctx: Send + Sync + 'static, T: Tool<Ctx> + ?Sized`.
- `ToolDef::from_tool` replaces the duplicate mapping in
  `crates/paigasus-helikon-core/src/agent.rs` (about line 912) and
  `crates/paigasus-helikon-runtime-temporal/src/worker.rs` (about line 402).
  It copies `provider_types` from the tool.

All 49 struct literals of `ToolDef` in 21 files change to the constructor:
core (`src/agent.rs`, `src/loop_state.rs`, `tests/wire_serde.rs`), anthropic
(9 in 6 files), bedrock (19 in 3 files), gemini (7 in 4 files), litellm (2),
openai `tests/live.rs` (2), runtime-temporal `src/worker.rs` (1), and
`tests/provider-stream-conformance/tests/conformance.rs` (6). The CLI has no
`ToolDef` literal. No code destructures `ToolDef`. The synthetic handoff
`ToolDef` in `loop_state.rs` has no provider types.

The facade needs no source change: `pub use paigasus_helikon_core as core;`
and core's `pub use model::*` already expose
`paigasus_helikon::core::ProviderToolType`. Do not add a second re-export.

### Anthropic provider (`src/translate/tools.rs`)

For each `ToolDef`:

1. Find the first entry in `provider_types` with `provider == "anthropic"`.
2. If there is one, and the builder did not set `native_tool_types(false)`,
   the tool entry is its `params` without a `cache_control` key. Then
   `"type"` is set to `type_name` and `"name"` is set to the `ToolDef` name.
   `type` and `name` are set last, so a `type` or `name` key in `params` cannot
   override them. The entry has no `description` and no `input_schema`, unless
   `params` contains those keys (they pass through, as documented). The
   `cache_control` key is removed because `translate/cache.rs` owns cache
   breakpoints, and an extra breakpoint can exceed the limit of four. A
   `tracing::debug!` on target `paigasus::anthropic::translate` records each
   typed entry.
3. If there is none, the entry is the generic entry, as today.

The crate exports `pub const PROVIDER_ID: &str = "anthropic";` and documents
the value. `AnthropicModel::provider()` (`src/model.rs:168`) returns
`PROVIDER_ID`, so the two cannot drift. A provider-agnostic tool crate can
write the literal `"anthropic"` instead of a dependency on this crate. A typo
such as `"Anthropic"` gives the generic entry with no error; the debug trace
helps to find it.

`AnthropicModelBuilder::native_tool_types(bool)` (default `true`) is the
opt-out. With `false`, every tool gets the generic entry. Use it for a model or
an Anthropic-compatible gateway that does not know a declared type.

The existing cache marker (`translate/cache.rs`) puts `cache_control` on the
last tool entry. The tool reference lists `cache_control` as an optional
property of Anthropic-provided tools, so no change is necessary.

The memory tool docs (fetched 2026-11-20) mark the tool `stable` and say: "the
memory tool itself doesn't require a beta header". The env-gated live test
below confirms this against the real API, with a typed entry plus
`cache_control`. If the API needs a header, the builder's existing `.beta(...)`
(`src/builder.rs:116`) is the documented way to send it.

### Runtime-temporal

`call_model` activity inputs carry `ModelRequest`, so `provider_types` goes
into Temporal history. A pre-0.8 activity worker in a rolling deploy reads the
new JSON (no `deny_unknown_fields`) but drops `provider_types`, so that turn
sends the generic entry. This is safe, but silent. The CHANGELOG notes it.

### Other providers

The OpenAI (Chat and Responses backends), Gemini, Bedrock and LiteLLM
translators do not read `provider_types`. Each gets one test: a `ToolDef` with
an `anthropic` provider type gives the same output as the same `ToolDef` with
no provider types.

### Tests

- Core: `ProviderToolType` serde round trip; an old `ToolDef` JSON without
  `provider_types` deserializes; a `ToolDef` with no provider types serializes
  with no `provider_types` key (extend `tests/wire_serde.rs`);
  `ToolDef::from_tool` copies all fields, including `provider_types`.
- Runtime-temporal: a frozen-literal test in `activity_input.rs` with one
  old-shape `ToolDef` and one `ToolDef` with `provider_types`; extend
  `register_accepts_plain_agent_and_snapshots_tool_defs` (`worker.rs:767`) to
  assert that `provider_types` survive `register()`.
- Anthropic: the typed entry for `memory_20250818` is exactly
  `{"type": "memory_20250818", "name": "memory"}`; `params` are merged; `type`
  and `name` in `params` do not override; a non-`anthropic` entry gives the
  generic entry; the first `anthropic` entry wins; `cache_control` in `params`
  is removed; `native_tool_types(false)` gives the generic entry; the cache
  marker lands on a typed entry when it is the last tool and there is no
  `response_format`; with a `response_format`, the marker lands on the
  synthesized tool (`translate/mod.rs:53-65`) and the typed entry is
  unchanged; `provider()` returns `PROVIDER_ID`; an end-to-end request test
  (the existing wiremock style) shows the typed entry in the request body.
- Anthropic live (env-gated, `tests/live.rs`): one request with the
  `memory_20250818` typed entry plus `cache_control` succeeds.
- Each other provider: the fallback test above.

### Documentation

- `docs/book/src/concepts/tools.md`: the `Tool` trait listing and a short
  section on provider tool types, with the memory example.
- `docs/book/src/concepts/model-providers.md`: one line per provider on which
  providers use the declaration.
- `crates/paigasus-helikon-providers-anthropic/README.md` (mandatory): a short
  section on native tool types, `PROVIDER_ID` and `native_tool_types`.
- `crates/paigasus-helikon-core/README.md`, if it shows the `Tool` surface.

### CHANGELOG upgrade notes

Under `## [Unreleased]` in core and the facade:

- Build a `ToolDef` with `ToolDef::new` or `ToolDef::from_tool`. A struct
  literal and exhaustive destructuring no longer compile.
- Upgrade core, the facade and the direct sibling dependencies together.
  Check with `cargo tree -d` that there is one copy of core.
- A tool wrapper must forward `Tool::provider_types`.
- The JSON of a `ToolDef` with no provider types does not change.

Under `## [Unreleased]` in each library dependent of core (see Constraints): a
one-line note that the crate requires core 0.8. The anthropic note also
describes the typed entry, and the runtime-temporal note describes the
mixed-fleet behaviour above.

## Constraints

- **Breaking change to core.** `ToolDef` gets a field and becomes
  `#[non_exhaustive]`. Core goes from 0.7.x to 0.8.0. The PR title uses the
  `!` marker, for example `feat(core)!: SMA-830 ...`.
- **Release mechanics (SMA-838 rule, no hand bump).** Core has a breaking
  change, and every library dependent exposes core types. release-plz gives a
  dependent with no own commit only a patch. So:
  1. The PR title is `feat(core)!: SMA-830 ...`. The repo squash-merges with
     the PR title as the commit title, so the `!` must be in the title.
  2. The PR adds an upgrade note under `## [Unreleased]` in the CHANGELOG of
     core, the facade, and each library dependent of core. The CLI is exempt
     (binary crate, internal library). Then the `!` commit touches each of
     those crate directories, and release-plz gives each a minor bump.
  3. No version changes in this PR. No hand bump. (Release-ascend caveats 1
     and 2 apply only to a PR that bumps its own version.)
  4. Before the PR opens, squash the branch in a scratch clone and run
     release-plz **v0.4.3** `release-plz update`. Record the version table in
     the PR body. Every library dependent must show a minor bump (0.x) and
     core must show 0.8.0.
  5. After the merge, check the release PR and its CI before it merges.
     SMA-838 PR 2 adds a guard (`scripts/check-breaking-cascade.sh`) that fails
     a release PR with a missing cascade.
- **Order with SMA-838.** SMA-838 PR 1 republishes the core 0.7 dependents by
  hand. SMA-830 must merge only after SMA-838 PR 1 has published, so that the
  version base is the republished one. Rebase on `main` before step 4.
- MSRV 1.94 and `missing_docs`: every new public item gets a `///` comment.
- Every CI gate in `CLAUDE.md` must pass. release-plz runs
  cargo-semver-checks by default; the break is expected and the minor bump
  matches it.

## Left to the builder

- The exact test file names and whether a test is a unit test or an
  integration test.
- Accessor methods on `ProviderToolType`, if the public fields are not enough.
- The wording of the book section.

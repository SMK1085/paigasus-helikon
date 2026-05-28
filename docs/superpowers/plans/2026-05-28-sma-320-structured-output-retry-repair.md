# SMA-320 Structured `output_type<T>` with retry/repair — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `.output_type::<T>()` honest — the agent loop constrains the model to `T`'s JSON Schema on a finalizing turn, validates the terminal output, repairs exactly once on failure, and the caller gets `RunResult<T>` directly via `collect_typed::<T>()`.

**Architecture:** All control flow stays in the pure `transition()` state machine (`loop_state.rs`); the `async_stream` driver in `agent.rs` only carries `OutputType` into `TransitionCtx` and appends any repair message the transition returns. Two phases: an unconstrained tool-calling loop, then a single constrained *finalizing* turn (+ at most one repair). Validation is serde-based (authoritative); `jsonschema` is **not** added (its MSRV 1.83 exceeds the workspace MSRV 1.75 — `schema_errors` come from serde error strings, exactly the spec's graceful-degradation path).

**Tech Stack:** Rust (workspace MSRV 1.75), `schemars` 1, `serde`/`serde_json`, `async-stream`, `tokio` (test), `insta` (snapshot tests).

**Spec:** `docs/superpowers/specs/2026-05-28-sma-320-structured-output-retry-repair-design.md`

---

## File structure

| File | Change | Responsibility |
|------|--------|----------------|
| `crates/paigasus-helikon-core/src/schema.rs` | **create** | Canonical `strict()` JSON-Schema normalizer + tests |
| `crates/paigasus-helikon-core/src/lib.rs` | modify | `pub mod schema;` (namespaced, no glob) |
| `crates/paigasus-helikon-core/src/agent.rs` | modify | `OutputType` name+validator; `AgentEvent::{RepairStarted, StructuredOutputFailed}`; `AgentError::InvalidStructuredOutput` fields; driver wiring |
| `crates/paigasus-helikon-core/src/loop_state.rs` | modify | `Finalizing`/`RepairingOutput` states; `TransitionCtx.output`; `TransitionOutcome.conversation_appends`; validate/repair transitions |
| `crates/paigasus-helikon-core/src/runner.rs` | modify | `RunResultStreaming::collect_typed::<T>()` |
| `crates/paigasus-helikon-providers-openai/src/translate/tools.rs` | modify | `to_strict_schema` delegates to `core::schema::strict` |
| `crates/paigasus-helikon/src/lib.rs` | modify | `pub mod schema { pub use … strict; }` re-export |
| `crates/paigasus-helikon/Cargo.toml` | modify | `[[example]]` entry (required-features) |
| `crates/paigasus-helikon/examples/leukemia_classifier.rs` | **create** | Runnable doc example (feature-gated) |
| `crates/paigasus-helikon-core/tests/collect_typed.rs` | **create** | `collect_typed` unit coverage |
| `crates/paigasus-helikon-core/tests/structured_output.rs` | **create** | AC#1 + AC#2 integration tests |
| `crates/paigasus-helikon-core/tests/transition_unit.rs` | modify | add `output: None` to the test `TransitionCtx` builder |

**Decision recorded (deviation from spec D3):** `jsonschema` is dropped (MSRV 1.83 > 1.75). `schema_errors` are built from serde error strings. This is the spec's documented degradation, not a scope change. The breaking reshape of `AgentError::InvalidStructuredOutput` is conveyed to release-plz via a `BREAKING CHANGE:` trailer on its commit (Task 4) — no manual `Cargo.toml` version edits; release-plz bumps `paigasus-helikon-core` 0.1.x → 0.2.0 on merge.

---

## Task 1: `core::schema::strict()` shared helper

**Files:**
- Create: `crates/paigasus-helikon-core/src/schema.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs:27` (add module)
- Modify: `crates/paigasus-helikon-providers-openai/src/translate/tools.rs:27-31`
- Modify: `crates/paigasus-helikon/src/lib.rs` (re-export)

- [ ] **Step 1: Write `schema.rs` with the helper and its tests**

Create `crates/paigasus-helikon-core/src/schema.rs` (logic lifted verbatim from OpenAI's current private `to_strict_schema`, now public and documented honestly):

```rust
//! JSON Schema strict-mode normalization.
//!
//! [`strict`] rewrites a schemars-produced JSON Schema to satisfy
//! **OpenAI strict-mode / JSON-Schema** requirements:
//! 1. `additionalProperties: false` on every object.
//! 2. Every key in each object's `properties` promoted into `required`
//!    (no truly-optional fields — `Option<T>` must use `"type": ["T", "null"]`
//!    and stay present in `required`; schemars 1.x emits this natively).
//!
//! This is **not** a provider-neutral transform: it encodes OpenAI's
//! strict-mode quirks. Per-provider normalization for future providers
//! (Bedrock/Gemini, untagged-enum collapsing) is a separate concern.
//! The OpenAI provider calls this; Anthropic uses schemas as-is.

use serde_json::Value;

/// Rewrite a JSON Schema for OpenAI strict-mode structured output.
///
/// Recursively sets `additionalProperties: false` on every object,
/// promotes every key in each object's `properties` into `required`, and
/// recurses into object `properties` and array `items`. Schemas that
/// would produce strict-mode rejections (hand-authored
/// `oneOf: [_, {type: "null"}]`, unsupported `pattern`, etc.) pass
/// through unmodified.
pub fn strict(value: &Value) -> Value {
    let mut out = value.clone();
    rewrite_in_place(&mut out);
    out
}

fn rewrite_in_place(v: &mut Value) {
    if let Some(obj) = v.as_object_mut() {
        let is_object_schema = obj
            .get("type")
            .and_then(|t| t.as_str())
            .map(|s| s == "object")
            .unwrap_or(false);

        if is_object_schema {
            obj.insert("additionalProperties".to_owned(), Value::Bool(false));

            if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
                let keys: Vec<String> = props.keys().cloned().collect();
                let required = Value::Array(keys.into_iter().map(Value::String).collect());
                obj.insert("required".to_owned(), required);
            }
        }

        if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
            for (_, child) in props.iter_mut() {
                rewrite_in_place(child);
            }
        }
        if let Some(items) = obj.get_mut("items") {
            rewrite_in_place(items);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_object_adds_additional_properties_false() {
        let input = json!({"type": "object", "properties": {"name": {"type": "string"}}});
        assert_eq!(strict(&input)["additionalProperties"], json!(false));
    }

    #[test]
    fn flat_object_promotes_all_keys_into_required() {
        let input = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "age": {"type": "integer"}}
        });
        let out = strict(&input);
        let mut keys: Vec<&str> = out["required"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["age", "name"]);
    }

    #[test]
    fn nested_object_gets_strict_treatment() {
        let input = json!({
            "type": "object",
            "properties": {"user": {"type": "object", "properties": {"id": {"type": "string"}}}}
        });
        let out = strict(&input);
        assert_eq!(out["properties"]["user"]["additionalProperties"], json!(false));
        assert_eq!(out["properties"]["user"]["required"].as_array().unwrap(), &vec![json!("id")]);
    }

    #[test]
    fn array_of_objects_recurses_into_items() {
        let input = json!({
            "type": "object",
            "properties": {"tags": {"type": "array",
                "items": {"type": "object", "properties": {"name": {"type": "string"}}}}}
        });
        assert_eq!(strict(&input)["properties"]["tags"]["items"]["additionalProperties"], json!(false));
    }

    #[test]
    fn explicit_additional_properties_true_is_overridden_to_false() {
        let input = json!({"type": "object", "additionalProperties": true, "properties": {"k": {"type": "string"}}});
        assert_eq!(strict(&input)["additionalProperties"], json!(false));
    }

    #[test]
    fn option_t_emitted_as_type_array_is_preserved() {
        let input = json!({
            "type": "object",
            "properties": {"since": {"type": ["string", "null"]}, "kind": {"type": "string"}}
        });
        let out = strict(&input);
        assert_eq!(out["properties"]["since"]["type"], json!(["string", "null"]));
        let mut req: Vec<String> = out["required"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap().to_owned()).collect();
        req.sort();
        assert_eq!(req, vec!["kind", "since"]);
    }
}
```

- [ ] **Step 2: Register the module (namespaced, no glob)**

In `crates/paigasus-helikon-core/src/lib.rs`, add after line 26 (`pub mod tool;`) — do **not** add a `pub use schema::*;` (it is accessed as `schema::strict`):

```rust
pub mod schema;
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p paigasus-helikon-core schema:: -- --nocapture`
Expected: PASS (6 tests).

- [ ] **Step 4: Repoint OpenAI's `to_strict_schema` to delegate**

In `crates/paigasus-helikon-providers-openai/src/translate/tools.rs`, replace the function body (lines 27-62, the `to_strict_schema` fn + `rewrite_in_place` helper) with a delegate. Keep the existing `#[cfg(test)] mod tests` block **unchanged** (it still exercises behavior through the delegate, including the `snapshot_complex_tool_args` insta snapshot):

```rust
/// Rewrite a JSON Schema for OpenAI strict-mode tool calls.
///
/// Delegates to [`paigasus_helikon_core::schema::strict`], the canonical
/// normalizer. Kept as a crate-private alias so existing call sites and
/// tests are unaffected.
pub(crate) fn to_strict_schema(value: &Value) -> Value {
    paigasus_helikon_core::schema::strict(value)
}
```

Delete the now-unused private `fn rewrite_in_place` in that file (the core module owns it). Keep the `use serde_json::Value;` import (still used by the signature + tests).

- [ ] **Step 5: Run OpenAI provider tests (snapshot must still pass)**

Run: `cargo test -p paigasus-helikon-providers-openai translate::tools`
Expected: PASS, including `snapshot_complex_tool_args` (no snapshot change — output is identical).

- [ ] **Step 6: Add the facade re-export**

In `crates/paigasus-helikon/src/lib.rs`, append at the end of the file:

```rust
/// JSON Schema helpers.
pub mod schema {
    /// OpenAI/JSON-Schema strict-mode normalizer — see
    /// [`paigasus_helikon_core::schema::strict`].
    pub use paigasus_helikon_core::schema::strict;
}
```

- [ ] **Step 7: Verify facade docs (the re-export needs a doc comment)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon --no-deps`
Expected: builds with no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-core/src/schema.rs \
        crates/paigasus-helikon-core/src/lib.rs \
        crates/paigasus-helikon-providers-openai/src/translate/tools.rs \
        crates/paigasus-helikon/src/lib.rs
git commit -m "feat(core): SMA-320 add canonical schema::strict helper"
```

---

## Task 2: `OutputType` gains `name` + validator

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs:106-125` (`OutputType`)

- [ ] **Step 1: Write failing tests for the new shape**

Append to the existing test module in `agent_builder.rs`? No — `OutputType` lives in `agent.rs`, which has no test module. Add a test module at the end of `crates/paigasus-helikon-core/src/agent.rs`:

```rust
#[cfg(test)]
mod output_type_tests {
    use super::OutputType;
    use serde_json::json;

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct Answer {
        value: u32,
    }

    #[test]
    fn from_schema_populates_name_and_schema() {
        let ot = OutputType::from_schema::<Answer>();
        assert_eq!(ot.name, "Answer");
        // schema is the schemars schema for Answer
        let v = serde_json::to_value(&ot.schema).unwrap();
        assert_eq!(v["properties"]["value"]["type"], json!("integer"));
    }

    #[test]
    fn validate_accepts_conformant_and_rejects_nonconformant() {
        let ot = OutputType::from_schema::<Answer>();
        assert!(ot.validate(&json!({"value": 7})).is_ok());
        let err = ot.validate(&json!({"value": "not a number"})).unwrap_err();
        assert!(!err.is_empty(), "expected at least one error string");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-core output_type_tests`
Expected: FAIL to compile — `OutputType` has no `name` field and no `validate` method.

- [ ] **Step 3: Rewrite `OutputType` (agent.rs:106-125)**

Replace the existing `OutputType` struct + impl block with:

```rust
/// Structured-output type marker: the JSON Schema the model is asked to
/// produce, the schema's name, and a validator that proves text
/// deserializes into the original `T`.
///
/// The validator is a function pointer captured at [`OutputType::from_schema`]
/// time (where `T: DeserializeOwned` is in scope). It is the authoritative
/// gate the agent loop uses to decide success vs. repair; the typed value
/// itself is materialized later by
/// [`crate::RunResultStreaming::collect_typed`].
#[derive(Clone)]
pub struct OutputType {
    /// The schema name (the `T` identifier / schema title). Echoed into the
    /// provider `response_format` name and into the repair instruction.
    pub name: String,
    /// The JSON Schema the model should produce (raw schemars output).
    pub schema: schemars::Schema,
    /// Authoritative validator: `Ok(())` iff the value deserializes into the
    /// original `T`; `Err` carries one or more human-readable error strings.
    validate: fn(&serde_json::Value) -> Result<(), Vec<String>>,
}

impl std::fmt::Debug for OutputType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputType")
            .field("name", &self.name)
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

impl OutputType {
    /// Construct from a type that derives [`schemars::JsonSchema`] and
    /// [`serde::de::DeserializeOwned`].
    ///
    /// Captures a validator that attempts `serde_json::from_value::<T>` and
    /// derives `name` from the schema's `title` (falling back to
    /// `"StructuredOutput"` if absent).
    pub fn from_schema<T>() -> Self
    where
        T: schemars::JsonSchema + serde::de::DeserializeOwned,
    {
        let schema = schemars::schema_for!(T);
        let name = schema
            .as_value()
            .get("title")
            .and_then(|t| t.as_str())
            .map(|s| s.to_owned())
            .unwrap_or_else(|| "StructuredOutput".to_owned());
        Self {
            schema,
            name,
            validate: |v| {
                serde_json::from_value::<T>(v.clone())
                    .map(|_| ())
                    .map_err(|e| vec![e.to_string()])
            },
        }
    }

    /// Run the captured validator against `value`.
    pub fn validate(&self, value: &serde_json::Value) -> Result<(), Vec<String>> {
        (self.validate)(value)
    }
}
```

Note: `schemars::Schema::as_value()` returns `&serde_json::Value` in schemars 1.x. If the exact accessor differs at build time, use `serde_json::to_value(&schema).ok().and_then(|v| v.get("title")…)` — but prefer `as_value()`.

- [ ] **Step 4: Tighten the builder bound (`from_schema` now needs `DeserializeOwned`)**

The builder's `.output_type::<T2>()` (`agent_builder.rs:381-384`) already bounds `T2: … + serde::de::DeserializeOwned + schemars::JsonSchema`, so it compiles unchanged. Confirm no other caller of `OutputType::from_schema` exists:

Run: `grep -rn "OutputType::from_schema" crates --include="*.rs"`
Expected: only `agent_builder.rs` and the new tests + `loop_happy_path.rs:72` (a `let _ = OutputType::from_schema::<String>;` smoke line — `String: DeserializeOwned + JsonSchema`, still compiles).

- [ ] **Step 5: Run tests**

Run: `cargo test -p paigasus-helikon-core output_type_tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Run the existing builder tests (schema-equality assertions still hold)**

Run: `cargo test -p paigasus-helikon-core --test '*' agent_builder 2>/dev/null; cargo test -p paigasus-helikon-core output_type`
Expected: PASS. (The builder unit tests in `agent_builder.rs` compare `agent.output_type.unwrap().schema` only — unaffected.)

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "feat(core): SMA-320 give OutputType a name and validator"
```

---

## Task 3: `RunResultStreaming::collect_typed::<T>()`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs` (imports + new method)
- Create: `crates/paigasus-helikon-core/tests/collect_typed.rs`

This depends on `AgentEvent::StructuredOutputFailed`, added in Task 4. To keep this task self-contained, the method is written to handle that variant; it is added to the enum here so this compiles, then Task 4 wires the driver to emit it.

- [ ] **Step 1: Add the two new `AgentEvent` variants (agent.rs)**

In `crates/paigasus-helikon-core/src/agent.rs`, inside `enum AgentEvent`, add under the `// --- Control ---` section (after `ApprovalRequested`):

```rust
    /// A structured-output repair turn has begun: validation of the prior
    /// constrained output failed and the loop is re-prompting once.
    RepairStarted {
        /// 1-based repair attempt index. Only ever `1` under the one-shot budget.
        attempt: u32,
    },
    /// Structured-output validation failed terminally (after the one repair
    /// attempt). Emitted immediately before the terminal [`AgentEvent::RunFailed`]
    /// so consumers can recover the structured detail.
    StructuredOutputFailed {
        /// Human-readable schema/validation errors.
        schema_errors: Vec<String>,
        /// The raw terminal assistant text that failed validation.
        final_text: String,
    },
```

- [ ] **Step 2: Write the failing test**

Create `crates/paigasus-helikon-core/tests/collect_typed.rs`:

```rust
//! collect_typed deserializes the terminal assistant text into T, and maps
//! a StructuredOutputFailed event to AgentError::InvalidStructuredOutput.

use futures_util::stream;
use paigasus_helikon_core::{
    AgentError, AgentEvent, ContentPart, Item, RunResultStreaming, TokenUsage,
};

#[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    value: u32,
}

#[tokio::test]
async fn collect_typed_returns_struct() {
    let events = vec![
        AgentEvent::MessageOutput {
            item: Item::AssistantMessage {
                content: vec![ContentPart::Text { text: "{\"value\":7}".into() }],
                agent: None,
            },
        },
        AgentEvent::RunCompleted { usage: TokenUsage::default() },
    ];
    let stream = Box::pin(stream::iter(events));
    let result = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect("collect_typed should succeed");
    assert_eq!(result.final_output, Answer { value: 7 });
}

#[tokio::test]
async fn collect_typed_maps_structured_failure() {
    let events = vec![
        AgentEvent::StructuredOutputFailed {
            schema_errors: vec!["missing field `value`".into()],
            final_text: "{}".into(),
        },
        AgentEvent::RunFailed { error: "invalid structured output".into() },
    ];
    let stream = Box::pin(stream::iter(events));
    let err = RunResultStreaming::new(stream)
        .collect_typed::<Answer>()
        .await
        .expect_err("should be an error");
    match err {
        AgentError::InvalidStructuredOutput { schema_errors, final_text } => {
            assert_eq!(schema_errors, vec!["missing field `value`".to_string()]);
            assert_eq!(final_text, "{}");
        }
        other => panic!("expected InvalidStructuredOutput, got {other:?}"),
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p paigasus-helikon-core --test collect_typed`
Expected: FAIL to compile — `collect_typed` does not exist (and `InvalidStructuredOutput` is still a unit variant; that is fixed in Task 4, so this step's failure may also cite the variant fields — that is fine, proceed).

> Note: this test fully passes only after Task 4 reshapes `InvalidStructuredOutput`. If you are running strictly TDD-green per task, defer running this test to the end of Task 4. The `collect_typed` method itself is implemented and committed here.

- [ ] **Step 4: Implement `collect_typed` (runner.rs)**

In `crates/paigasus-helikon-core/src/runner.rs`, extend the `use crate::{…}` import at the top to include `ContentPart` and `Item`:

```rust
use crate::{Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext};
```

Add to `impl RunResultStreaming` (after `collect`):

```rust
    /// Drain the stream and deserialize the terminal assistant text into `T`.
    ///
    /// The terminal output is the concatenated text of the last
    /// [`AgentEvent::MessageOutput`]. On a successful structured run the agent
    /// loop has already validated that text against `T`, so the parse here
    /// cannot fail. A failed run surfaces the underlying [`AgentError`]:
    /// structured-validation failures (carried by
    /// [`AgentEvent::StructuredOutputFailed`]) become
    /// [`AgentError::InvalidStructuredOutput`]; any other terminal
    /// [`AgentEvent::RunFailed`] becomes [`AgentError::Other`].
    pub async fn collect_typed<T>(mut self) -> Result<RunResult<T>, AgentError>
    where
        T: serde::de::DeserializeOwned,
    {
        use futures_util::stream::StreamExt;
        let mut events = Vec::new();
        let mut final_text = String::new();
        let mut usage = crate::TokenUsage::default();
        let mut structured_err: Option<(Vec<String>, String)> = None;

        while let Some(ev) = self.events.next().await {
            match &ev {
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage { content, .. },
                } => {
                    final_text.clear();
                    for part in content {
                        if let ContentPart::Text { text } = part {
                            final_text.push_str(text);
                        }
                    }
                }
                AgentEvent::RunCompleted { usage: u } => usage = *u,
                AgentEvent::StructuredOutputFailed {
                    schema_errors,
                    final_text: ft,
                } => {
                    structured_err = Some((schema_errors.clone(), ft.clone()));
                }
                AgentEvent::RunFailed { error } => {
                    let error = error.clone();
                    events.push(ev);
                    if let Some((schema_errors, final_text)) = structured_err {
                        return Err(AgentError::InvalidStructuredOutput {
                            schema_errors,
                            final_text,
                        });
                    }
                    return Err(AgentError::Other(anyhow::anyhow!(error)));
                }
                _ => {}
            }
            events.push(ev);
        }

        let final_output = serde_json::from_str::<T>(final_text.trim()).map_err(|e| {
            AgentError::Other(anyhow::anyhow!(
                "collect_typed: failed to deserialize final output: {e}"
            ))
        })?;
        Ok(RunResult {
            final_output,
            events,
            usage,
        })
    }
```

- [ ] **Step 5: Build (test run deferred to Task 4)**

Run: `cargo build -p paigasus-helikon-core`
Expected: compiles. (`collect_typed` references `InvalidStructuredOutput { schema_errors, final_text }`, which is reshaped in Task 4 — if you implement tasks in order, this build fails on the variant shape; that is expected. Implement Task 4's enum change first if you prefer strict per-task green, OR fold Steps here with Task 4 Step 1. The chosen order: do Task 4 Step 1 now, then return.)

> **Practical ordering note:** the enum reshape (Task 4 Step 1) and this method are interdependent. Recommended micro-order: Task 4 Step 1 (reshape variant) → Task 3 Step 4 (this method) → run `collect_typed` tests. Commit both together if implementing inline.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs \
        crates/paigasus-helikon-core/src/agent.rs \
        crates/paigasus-helikon-core/tests/collect_typed.rs
git commit -m "feat(core): SMA-320 add RunResultStreaming::collect_typed"
```

---

## Task 4: Reshape `AgentError::InvalidStructuredOutput` (breaking)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs:668-672`

- [ ] **Step 1: Reshape the variant**

In `crates/paigasus-helikon-core/src/agent.rs`, replace the unit variant:

```rust
    /// The model produced output that could not be coerced into the
    /// requested structured type, even after the one-shot repair attempt
    /// allowed by ADR-10.
    #[error("invalid structured output after one repair attempt: {schema_errors:?}")]
    InvalidStructuredOutput {
        /// Human-readable schema/validation errors.
        schema_errors: Vec<String>,
        /// The raw terminal assistant text that failed validation.
        final_text: String,
    },
```

- [ ] **Step 2: Build the whole workspace**

Run: `cargo build --workspace`
Expected: compiles. (No in-repo site constructs or matches the old unit variant — verified: only doc-comments in `tool.rs` reference it.)

- [ ] **Step 3: Run the collect_typed tests (now green)**

Run: `cargo test -p paigasus-helikon-core --test collect_typed`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit (with breaking-change trailer for release-plz)**

```bash
git add crates/paigasus-helikon-core/src/agent.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-320 carry schema_errors and final_text on InvalidStructuredOutput

BREAKING CHANGE: AgentError::InvalidStructuredOutput is now a struct
variant { schema_errors, final_text } instead of a unit variant.
EOF
)"
```

---

## Task 5: State-machine scaffolding (states, ctx field, outcome field, driver wiring)

No behavior change yet — every new construction site gets a neutral default so the existing suite stays green.

**Files:**
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (driver)
- Modify: `crates/paigasus-helikon-core/tests/transition_unit.rs:27`

- [ ] **Step 1: Add `output` to `TransitionCtx` (loop_state.rs:156-167)**

Add a field to the struct:

```rust
pub struct TransitionCtx<'a> {
    /// Tool definitions available this run.
    pub tools: &'a [ToolDef],
    /// Provider-tuning knobs.
    pub model_settings: &'a ModelSettings,
    /// Maximum number of turns before the loop fails.
    pub max_turns: u32,
    /// The driver's accumulated conversation so far.
    pub conversation: &'a [Item],
    /// Structured-output type, when the agent configured one. Drives the
    /// constrained finalizing turn and output validation.
    pub output: Option<&'a crate::OutputType>,
}
```

- [ ] **Step 2: Add `conversation_appends` to `TransitionOutcome` (loop_state.rs:170-178)**

```rust
#[derive(Debug)]
pub struct TransitionOutcome {
    /// The state after this step.
    pub next_state: LoopState,
    /// Events to yield through the driver's event stream.
    pub events: Vec<AgentEvent>,
    /// Side effect the driver must run before the next step.
    pub next_action: NextAction,
    /// Items the driver must append to its owned conversation before the
    /// next step (e.g. a synthesized repair message). Empty in most arms.
    pub conversation_appends: Vec<Item>,
}
```

- [ ] **Step 3: Add the two new `LoopState` variants (loop_state.rs:23-61)**

Add before `Done`:

```rust
    /// Constrained finalizing turn: the model is asked to emit the
    /// structured output for the configured `output_type`.
    Finalizing {
        /// The turn index that produced this finalizing request.
        turn: u32,
    },
    /// The one allowed repair turn after a failed finalizing validation.
    RepairingOutput {
        /// The turn index of the finalizing turn being repaired.
        turn: u32,
    },
```

- [ ] **Step 4: Add `conversation_appends: Vec::new()` to every existing `TransitionOutcome` literal**

In `loop_state.rs`, every `TransitionOutcome { … }` and the `not_implemented` helper and the `(s, i)` catch-all must add the field. The construction sites are at (current) lines ~194, 210, 242, 271, 295, 307, 318, 332. Add `conversation_appends: Vec::new(),` to each. Example for the max-turns arm:

```rust
        (LoopState::CallingModel { turn }, _) if *turn >= ctx.max_turns => TransitionOutcome {
            next_state: LoopState::Failed(AgentError::MaxTurnsExceeded(ctx.max_turns)),
            events: vec![AgentEvent::RunFailed {
                error: format!("max turns ({}) exceeded", ctx.max_turns),
            }],
            next_action: NextAction::Terminate,
            conversation_appends: Vec::new(),
        },
```

Do the same for the Start arm, the tool-calls arm, the no-tool-calls (Done) arm, the tool-results `return` arm, the tool-results normal arm, the catch-all `(s, i)` arm, and the `not_implemented` helper.

- [ ] **Step 5: Update the driver to pass `output` and apply `conversation_appends` (agent.rs)**

In `crates/paigasus-helikon-core/src/agent.rs`:

(a) In the snapshot block (after `let agent_name = self.name.clone();`, ~line 492) add:

```rust
        let output_type = self.output_type.clone();
```

(b) In the `TransitionCtx` literal (line 519-524) add the `output` field:

```rust
                let tx_ctx = crate::TransitionCtx {
                    tools: &tool_defs,
                    model_settings: &model_settings,
                    max_turns,
                    conversation: &conversation,
                    output: output_type.as_ref(),
                };
```

(c) Update the outcome destructure + apply appends (line 526-528):

```rust
                let crate::TransitionOutcome { next_state, events, next_action, conversation_appends } = outcome;
                for ev in events { yield ev; }
                loop_state = next_state;
                conversation.extend(conversation_appends);
```

- [ ] **Step 6: Update the test `TransitionCtx` builder (transition_unit.rs:27)**

In `crates/paigasus-helikon-core/tests/transition_unit.rs`, add `output: None,` to the `TransitionCtx { … }` returned by the helper at line 27.

- [ ] **Step 7: Build + run the full core suite (no behavior change → all green)**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (existing loop/transition tests unaffected; new `Finalizing`/`RepairingOutput` states are unreachable so far).

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs \
        crates/paigasus-helikon-core/src/agent.rs \
        crates/paigasus-helikon-core/tests/transition_unit.rs
git commit -m "refactor(core): SMA-320 scaffold finalizing/repair states and ctx wiring"
```

---

## Task 6: Constrain + validate — no-tools happy path (AC#1, no tools)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`
- Create: `crates/paigasus-helikon-core/tests/structured_output.rs`

- [ ] **Step 1: Add private helpers to `loop_state.rs`**

At the bottom of `loop_state.rs` (near `not_implemented`), add. Also extend the top-of-file `use crate::{…}` to include `ContentPart` and `ResponseFormat` (ContentPart is already imported; add `ResponseFormat`):

```rust
/// Build constrained model settings for a finalizing/repair turn: inject the
/// `output_type`-derived `response_format` (raw schema, strict mode) and clear
/// any caller tool_choice (Anthropic forces its own synthesized tool).
fn constrained_settings(base: &ModelSettings, output: &crate::OutputType) -> ModelSettings {
    let mut s = base.clone();
    s.response_format = Some(ResponseFormat::JsonSchema {
        name: output.name.clone(),
        schema: serde_json::to_value(&output.schema).unwrap_or(serde_json::Value::Null),
        strict: true,
    });
    s.tool_choice = None;
    s
}

/// Concatenate `ContentPart::Text` parts (the structured output arrives as text
/// on both providers).
fn flatten_text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Parse + validate terminal text against the output type.
/// `Ok(())` on success; `Err(schema_errors)` otherwise (non-JSON included).
fn validate_terminal(output: &crate::OutputType, content: &[ContentPart]) -> Result<(), Vec<String>> {
    let text = flatten_text(content);
    let value: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(e) => return Err(vec![format!("response was not valid JSON: {e}")]),
    };
    output.validate(&value)
}

/// The last `AssistantMessage` content in a list of items.
fn last_assistant_content(items: &[Item]) -> Vec<ContentPart> {
    items
        .iter()
        .rev()
        .find_map(|i| match i {
            Item::AssistantMessage { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}
```

- [ ] **Step 2: Constrain turn 0 for no-tools agents (Start arm, loop_state.rs ~202-215)**

Replace the Start arm with a version that enters `Finalizing` directly when an output type is set and there are no tools:

```rust
        (LoopState::CallingModel { turn }, TransitionInput::Start { .. })
            if *turn < ctx.max_turns =>
        {
            match ctx.output {
                Some(out) if ctx.tools.is_empty() => {
                    let request = ModelRequest {
                        messages: ctx.conversation.to_vec(),
                        tools: Vec::new(),
                        model_settings: constrained_settings(ctx.model_settings, out),
                    };
                    TransitionOutcome {
                        next_state: LoopState::Finalizing { turn: *turn },
                        events: vec![AgentEvent::TurnStarted { turn: *turn }],
                        next_action: NextAction::CallModel { request },
                        conversation_appends: Vec::new(),
                    }
                }
                _ => {
                    let request = ModelRequest {
                        messages: ctx.conversation.to_vec(),
                        tools: ctx.tools.to_vec(),
                        model_settings: ctx.model_settings.clone(),
                    };
                    TransitionOutcome {
                        next_state: LoopState::CallingModel { turn: *turn },
                        events: vec![AgentEvent::TurnStarted { turn: *turn }],
                        next_action: NextAction::CallModel { request },
                        conversation_appends: Vec::new(),
                    }
                }
            }
        }
```

- [ ] **Step 3: Add the `Finalizing` validation arm (loop_state.rs)**

Add a new arm before the `ApplyingHandoff`/catch-all arms. On a no-tool-call response, validate; success → `Done`; failure → repair (which is implemented in Task 8 — for now, route failure to `Failed(InvalidStructuredOutput…)` so this arm is total; Task 8 replaces the failure branch with the repair transition):

```rust
        (LoopState::Finalizing { turn }, TransitionInput::ModelResponse { items, usage, .. }) => {
            let Some(out) = ctx.output else {
                return TransitionOutcome {
                    next_state: LoopState::Failed(AgentError::Other(anyhow::anyhow!(
                        "Finalizing state without output type"
                    ))),
                    events: vec![AgentEvent::RunFailed {
                        error: "internal: Finalizing without output type".to_owned(),
                    }],
                    next_action: NextAction::Terminate,
                    conversation_appends: Vec::new(),
                };
            };
            let mut events: Vec<AgentEvent> = items
                .iter()
                .filter(|i| matches!(i, Item::AssistantMessage { .. }))
                .cloned()
                .map(|item| AgentEvent::MessageOutput { item })
                .collect();
            let content = last_assistant_content(&items);
            let has_tool_call = items.iter().any(|i| matches!(i, Item::ToolCall { .. }));

            let validation = if has_tool_call {
                Err(vec![
                    "model called a tool on the constrained finalizing turn".to_owned(),
                ])
            } else {
                validate_terminal(out, &content)
            };

            match validation {
                Ok(()) => {
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
                Err(schema_errors) => {
                    // Task 8 replaces this branch with the one-shot repair transition.
                    let final_text = flatten_text(&content);
                    events.push(AgentEvent::StructuredOutputFailed {
                        schema_errors: schema_errors.clone(),
                        final_text: final_text.clone(),
                    });
                    events.push(AgentEvent::RunFailed {
                        error: "invalid structured output".to_owned(),
                    });
                    let _ = turn;
                    TransitionOutcome {
                        next_state: LoopState::Failed(AgentError::InvalidStructuredOutput {
                            schema_errors,
                            final_text,
                        }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
            }
        }
```

- [ ] **Step 4: Write the AC#1 (no-tools) integration test**

Create `crates/paigasus-helikon-core/tests/structured_output.rs`:

```rust
//! SMA-320 structured output: AC#1 (typed struct returned) and AC#2
//! (one repair then error), plus the tools two-phase path.

#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use paigasus_helikon_core::{
    Agent, AgentInput, Instructions, LlmAgent, ModelEvent, FinishReason, ModelSettings,
    RunConfig, RunResultStreaming,
};

use common::{noop_run_context, MockModel};

#[derive(Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
struct LeukemiaSubtypeAnalysis {
    subtype: String,
    confidence: u32,
}

fn agent_with_output<M>(model: Arc<M>) -> LlmAgent<(), M, LeukemiaSubtypeAnalysis>
where
    M: paigasus_helikon_core::Model + 'static,
{
    LlmAgent::builder::<()>()
        .name("classifier")
        .shared_model(model)
        .instructions("Classify the sample.")
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build()
}

#[tokio::test]
async fn no_tools_structured_output_returns_struct() {
    let model = MockModel::with_scripts(vec![vec![
        ModelEvent::TokenDelta {
            text: "{\"subtype\":\"AML\",\"confidence\":92}".into(),
        },
        ModelEvent::Finish { reason: FinishReason::Stop },
    ]]);
    let agent = agent_with_output(model);
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("sample data"))
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect("collect_typed succeeds");
    assert_eq!(
        result.final_output,
        LeukemiaSubtypeAnalysis { subtype: "AML".into(), confidence: 92 }
    );
    let _ = (ModelSettings::new(), RunConfig::new(), Instructions::render);
}
```

(The final `let _ = …` line is a no-op import anchor; remove it if clippy flags it — see Step 6.)

- [ ] **Step 5: Run the no-tools test**

Run: `cargo test -p paigasus-helikon-core --test structured_output no_tools_structured_output_returns_struct`
Expected: PASS.

- [ ] **Step 6: Clean unused imports / clippy**

Run: `cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings`
Expected: no warnings. Remove any unused imports the anchor line was guarding (delete the `let _ = …` line and trim the `use` to what's actually used: `Agent, AgentInput, LlmAgent, ModelEvent, FinishReason, RunResultStreaming`).

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs \
        crates/paigasus-helikon-core/tests/structured_output.rs
git commit -m "feat(core): SMA-320 constrain and validate no-tools structured output"
```

---

## Task 7: Tools two-phase — finalizing turn after the tool loop (AC#1, tools)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs` (no-tool-calls arm)
- Modify: `crates/paigasus-helikon-core/tests/structured_output.rs`

- [ ] **Step 1: Route the no-tool-call response to `Finalizing` when output is set**

In `loop_state.rs`, the existing arm "Model produced a response with no tool calls → terminate" (lines ~252-276) currently always goes to `Done`. Replace it to branch on `ctx.output`:

```rust
        (LoopState::CallingModel { turn }, TransitionInput::ModelResponse { items, usage, .. })
            if !items.iter().any(|i| matches!(i, Item::ToolCall { .. })) =>
        {
            let mut events: Vec<AgentEvent> = items
                .iter()
                .filter(|i| matches!(i, Item::AssistantMessage { .. }))
                .cloned()
                .map(|item| AgentEvent::MessageOutput { item })
                .collect();

            match ctx.output {
                Some(out) => {
                    // Phase 2: issue one constrained finalizing turn (real tools
                    // withdrawn; the prior unconstrained answer stays in context).
                    let request = ModelRequest {
                        messages: ctx.conversation.to_vec(),
                        tools: Vec::new(),
                        model_settings: constrained_settings(ctx.model_settings, out),
                    };
                    events.push(AgentEvent::TurnStarted { turn: *turn });
                    TransitionOutcome {
                        next_state: LoopState::Finalizing { turn: *turn },
                        events,
                        next_action: NextAction::CallModel { request },
                        conversation_appends: Vec::new(),
                    }
                }
                None => {
                    let content = last_assistant_content(&items);
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
            }
        }
```

- [ ] **Step 2: Write the tools two-phase test**

Add to `crates/paigasus-helikon-core/tests/structured_output.rs`. Use `common::MockTool` for a real tool, and a `MockModel` scripted with: turn 0 → a tool call; turn 1 (after tool result) → plain text "done"; turn 2 (the constrained finalizing turn) → the JSON. Extend imports with `MockTool` and `ContentPart`/`Item` are not needed; we assert via `collect_typed`.

```rust
#[tokio::test]
async fn tools_two_phase_structured_output() {
    use common::MockTool;

    let tool = MockTool::new("fetch_panel", serde_json::json!({"blasts": 80}));
    let model = MockModel::with_scripts(vec![
        // turn 0: call the tool
        vec![
            ModelEvent::ToolCallDelta {
                call_id: "c1".into(),
                name: Some("fetch_panel".into()),
                args_delta: "{}".into(),
            },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        // turn 1: unconstrained free-text answer, no tool call
        vec![
            ModelEvent::TokenDelta { text: "Based on the panel, AML.".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
        // turn 2: constrained finalizing turn → structured JSON
        vec![
            ModelEvent::TokenDelta { text: "{\"subtype\":\"AML\",\"confidence\":88}".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);

    let agent = LlmAgent::builder::<()>()
        .name("classifier")
        .shared_model(model)
        .instructions("Classify the sample.")
        .shared_tool(tool.clone())
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build();

    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("sample"))
        .await
        .expect("run starts");
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect("collect_typed succeeds");

    assert_eq!(
        result.final_output,
        LeukemiaSubtypeAnalysis { subtype: "AML".into(), confidence: 88 }
    );
    assert_eq!(tool.invocations().len(), 1, "the real tool must run in phase 1");
}
```

- [ ] **Step 3: Run the tools test**

Run: `cargo test -p paigasus-helikon-core --test structured_output tools_two_phase_structured_output`
Expected: PASS — the real tool ran once, and the finalizing turn produced the struct.

- [ ] **Step 4: Run the full core suite (no regressions to the no-output path)**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (the existing no-output `loop_happy_path` / `loop_parallel_tools` tests still terminate via `Done`).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs \
        crates/paigasus-helikon-core/tests/structured_output.rs
git commit -m "feat(core): SMA-320 add two-phase finalizing turn for tools agents"
```

---

## Task 8: One-shot repair, then fail (AC#2) + tool-call-in-finalizing violation

**Files:**
- Modify: `crates/paigasus-helikon-core/src/loop_state.rs`
- Modify: `crates/paigasus-helikon-core/tests/structured_output.rs`

- [ ] **Step 1: Add the repair-message helper (loop_state.rs)**

Add near the other helpers:

```rust
/// Synthesize the one repair instruction sent back to the model.
fn repair_message(name: &str, errors: &[String]) -> Item {
    let text = format!(
        "Your previous response did not match the required `{name}` schema. \
         Errors: {}. Reply with ONLY a JSON value matching the schema — \
         no prose, no code fences.",
        errors.join("; ")
    );
    Item::UserMessage {
        content: vec![ContentPart::Text { text }],
    }
}
```

- [ ] **Step 2: Replace the `Finalizing` failure branch with the repair transition**

In the `Finalizing` arm added in Task 6, replace the `Err(schema_errors) => { … Failed … }` branch with a transition to `RepairingOutput` that re-prompts once:

```rust
                Err(schema_errors) => {
                    let msg = repair_message(&out.name, &schema_errors);
                    let mut messages = ctx.conversation.to_vec();
                    messages.push(msg.clone());
                    let request = ModelRequest {
                        messages,
                        tools: Vec::new(),
                        model_settings: constrained_settings(ctx.model_settings, out),
                    };
                    events.push(AgentEvent::RepairStarted { attempt: 1 });
                    TransitionOutcome {
                        next_state: LoopState::RepairingOutput { turn: *turn },
                        events,
                        next_action: NextAction::CallModel { request },
                        conversation_appends: vec![msg],
                    }
                }
```

- [ ] **Step 3: Add the `RepairingOutput` arm (terminal: success or fail)**

Add a new arm. Success → `Done`; failure (or a tool call) → `Failed(InvalidStructuredOutput…)` with the `StructuredOutputFailed` + `RunFailed` events:

```rust
        (LoopState::RepairingOutput { .. }, TransitionInput::ModelResponse { items, usage, .. }) => {
            let Some(out) = ctx.output else {
                return TransitionOutcome {
                    next_state: LoopState::Failed(AgentError::Other(anyhow::anyhow!(
                        "RepairingOutput state without output type"
                    ))),
                    events: vec![AgentEvent::RunFailed {
                        error: "internal: RepairingOutput without output type".to_owned(),
                    }],
                    next_action: NextAction::Terminate,
                    conversation_appends: Vec::new(),
                };
            };
            let mut events: Vec<AgentEvent> = items
                .iter()
                .filter(|i| matches!(i, Item::AssistantMessage { .. }))
                .cloned()
                .map(|item| AgentEvent::MessageOutput { item })
                .collect();
            let content = last_assistant_content(&items);
            let has_tool_call = items.iter().any(|i| matches!(i, Item::ToolCall { .. }));

            let validation = if has_tool_call {
                Err(vec![
                    "model called a tool on the repair turn".to_owned(),
                ])
            } else {
                validate_terminal(out, &content)
            };

            match validation {
                Ok(()) => {
                    events.push(AgentEvent::RunCompleted { usage });
                    TransitionOutcome {
                        next_state: LoopState::Done(FinalOutput { content, usage }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
                Err(schema_errors) => {
                    let final_text = flatten_text(&content);
                    events.push(AgentEvent::StructuredOutputFailed {
                        schema_errors: schema_errors.clone(),
                        final_text: final_text.clone(),
                    });
                    events.push(AgentEvent::RunFailed {
                        error: "invalid structured output after one repair attempt".to_owned(),
                    });
                    TransitionOutcome {
                        next_state: LoopState::Failed(AgentError::InvalidStructuredOutput {
                            schema_errors,
                            final_text,
                        }),
                        events,
                        next_action: NextAction::Terminate,
                        conversation_appends: Vec::new(),
                    }
                }
            }
        }
```

- [ ] **Step 4: Write the AC#2 test (one repair, then error)**

Add to `tests/structured_output.rs`. Script invalid JSON on the finalizing turn and invalid again on the repair turn; assert exactly one `RepairStarted`, a terminal failure, and that `collect_typed` returns `InvalidStructuredOutput`. Extend imports with `AgentEvent`, `AgentError`.

```rust
#[tokio::test]
async fn invalid_output_repairs_once_then_errors() {
    use paigasus_helikon_core::{AgentError, AgentEvent};

    let model = MockModel::with_scripts(vec![
        // finalizing turn: invalid (missing `confidence`)
        vec![
            ModelEvent::TokenDelta { text: "{\"subtype\":\"AML\"}".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
        // repair turn: still invalid (not even JSON)
        vec![
            ModelEvent::TokenDelta { text: "sorry, I cannot".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let agent = agent_with_output(model);
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("sample"))
        .await
        .expect("run starts");

    // Collect raw events first to assert the repair count.
    let events: Vec<AgentEvent> = {
        use futures_util::stream::StreamExt;
        stream.collect::<Vec<_>>().await
    };
    let repair_count = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::RepairStarted { .. }))
        .count();
    assert_eq!(repair_count, 1, "exactly one repair turn");
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::StructuredOutputFailed { .. })),
        "a StructuredOutputFailed event must be emitted"
    );

    // Re-run to assert the typed error surface (fresh scripts).
    let model2 = MockModel::with_scripts(vec![
        vec![
            ModelEvent::TokenDelta { text: "{\"subtype\":\"AML\"}".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
        vec![
            ModelEvent::TokenDelta { text: "still wrong".into() },
            ModelEvent::Finish { reason: FinishReason::Stop },
        ],
    ]);
    let agent2 = agent_with_output(model2);
    let stream2 = agent2
        .run(noop_run_context::<()>(), AgentInput::from_user_text("sample"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream2)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect_err("must error");
    match err {
        AgentError::InvalidStructuredOutput { schema_errors, final_text } => {
            assert!(!schema_errors.is_empty());
            assert_eq!(final_text, "still wrong");
        }
        other => panic!("expected InvalidStructuredOutput, got {other:?}"),
    }
}
```

- [ ] **Step 5: Run the AC#2 test**

Run: `cargo test -p paigasus-helikon-core --test structured_output invalid_output_repairs_once_then_errors`
Expected: PASS.

- [ ] **Step 6: Add the tool-call-in-finalizing violation test**

Add to `tests/structured_output.rs` — the finalizing turn returns a tool call (a violation); since real tools were withdrawn this is non-conforming and must repair, then (still a tool call) fail:

```rust
#[tokio::test]
async fn tool_call_on_finalizing_turn_is_a_violation() {
    use paigasus_helikon_core::{AgentError, AgentEvent};

    // No tools on the agent, so turn 0 is the finalizing turn. The model
    // (mis)behaves by emitting a tool call on both the finalizing and repair turns.
    let model = MockModel::with_scripts(vec![
        vec![
            ModelEvent::ToolCallDelta { call_id: "x".into(), name: Some("nope".into()), args_delta: "{}".into() },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
        vec![
            ModelEvent::ToolCallDelta { call_id: "y".into(), name: Some("nope".into()), args_delta: "{}".into() },
            ModelEvent::Finish { reason: FinishReason::ToolCalls },
        ],
    ]);
    let agent = agent_with_output(model);
    let stream = agent
        .run(noop_run_context::<()>(), AgentInput::from_user_text("sample"))
        .await
        .expect("run starts");
    let err = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await
        .expect_err("must error");
    assert!(matches!(err, AgentError::InvalidStructuredOutput { .. }));
    let _ = AgentEvent::RepairStarted { attempt: 1 }; // import anchor
}
```

> Note: the finalizing request has `tools: Vec::new()`, so `run_tools_concurrent` is never reached for the synthesized non-conforming tool call — the transition's `Finalizing`/`RepairingOutput` arms see the `ToolCall` item and treat it as a violation directly. Confirm the driver still feeds the `ModelResponse` (with the `ToolCall` item) into `transition` rather than routing to `ExecuteTools`: it does, because routing to `ExecuteTools` only happens from the `CallingModel` arm, not `Finalizing`/`RepairingOutput`.

- [ ] **Step 7: Run the violation test + full suite**

Run: `cargo test -p paigasus-helikon-core`
Expected: PASS (all structured_output tests + existing suite).

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-core/src/loop_state.rs \
        crates/paigasus-helikon-core/tests/structured_output.rs
git commit -m "feat(core): SMA-320 one-shot output repair then typed failure"
```

---

## Task 9: Leukemia example + docs + full CI sweep

**Files:**
- Create: `crates/paigasus-helikon/examples/leukemia_classifier.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (`[[example]]`)

- [ ] **Step 1: Add the feature-gated example entry to the facade Cargo.toml**

In `crates/paigasus-helikon/Cargo.toml`, after the `[dev-dependencies]` block add:

```toml
[[example]]
name              = "leukemia_classifier"
required-features = ["anthropic"]
```

- [ ] **Step 2: Write the example**

Create `crates/paigasus-helikon/examples/leukemia_classifier.rs`. It must **compile** under `--all-features` (CI builds examples) but only does network I/O at runtime:

```rust
//! Structured-output example (SMA-320): a classifier that returns a typed
//! struct directly. Run with an Anthropic key:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-… cargo run -p paigasus-helikon \
//!     --features anthropic --example leukemia_classifier
//! ```

use paigasus_helikon::core::{Agent, AgentInput, LlmAgent, RunResultStreaming};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct LeukemiaSubtypeAnalysis {
    /// e.g. "AML", "ALL", "CLL", "CML".
    subtype: String,
    /// 0–100.
    confidence: u32,
    /// One-sentence rationale.
    rationale: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model = paigasus_helikon::anthropic::AnthropicModel::builder()
        .model("claude-sonnet-4-6")
        .build()?;

    let agent = LlmAgent::builder::<()>()
        .name("leukemia-classifier")
        .model(model)
        .instructions("You are a hematopathology assistant. Classify the leukemia subtype.")
        .output_type::<LeukemiaSubtypeAnalysis>()
        .build();

    let ctx = paigasus_helikon::core::RunContext::ephemeral(());
    let input = AgentInput::from_user_text(
        "Flow: CD13+ CD33+ CD34+ MPO+. Blasts 80%. Auer rods present.",
    );

    let stream = agent.run(ctx, input).await?;
    let result = RunResultStreaming::new(stream)
        .collect_typed::<LeukemiaSubtypeAnalysis>()
        .await?;

    println!("{:#?}", result.final_output);
    Ok(())
}
```

> The example references `AnthropicModel::builder()` and `RunContext::ephemeral`. Verify the exact constructor names against the provider/context crates before finalizing; if `ephemeral` does not exist, build the `RunContext` with the same helper the integration tests use. Adjust the two constructor calls to the real APIs — the example must compile under `--features anthropic`. This is the only step that may need a name fix; everything else is API-stable.

- [ ] **Step 3: Verify the example compiles under the feature**

Run: `cargo build -p paigasus-helikon --features anthropic --example leukemia_classifier`
Expected: compiles. Fix the two constructor names if the build complains (see note in Step 2).

- [ ] **Step 4: Full local CI gate sweep (matches `.github/workflows/ci.yml`)**

Run each; all must pass:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```

Expected: all PASS. New `pub` items (`schema::strict`, facade `schema`, `OutputType` fields, `collect_typed`, the two `AgentEvent` variants, the reshaped `AgentError` fields) all carry `///` docs, so docs + coverage stay green.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon/Cargo.toml \
        crates/paigasus-helikon/examples/leukemia_classifier.rs
git commit -m "docs(core): SMA-320 add leukemia classifier structured-output example"
```

- [ ] **Step 6: Push and open the PR**

```bash
git push -u origin feature/sma-320-structured-output_typet-with-retryrepair
gh pr create \
  --title "feat(core): SMA-320 honor output_type<T> with structured validation and one-shot repair" \
  --body "Implements SMA-320 per docs/superpowers/specs/2026-05-28-sma-320-structured-output-retry-repair-design.md.

Note: contains a BREAKING CHANGE to AgentError::InvalidStructuredOutput (unit → struct variant); release-plz will bump paigasus-helikon-core to 0.2.0.

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
```

PR title satisfies both `pr-title.yml` rules: full `feat(core):` Conventional-Commit prefix, lowercase subject after `SMA-320`.

---

## Post-merge follow-up (not a code task)

- **Notion doc-sync (required by the spec):** update the "Structured Output & Builder" page so its example shows the SMA-320 MVP form `agent.run(…).collect_typed::<T>()` and labels the `runner.run(&agent, …) -> RunResult<T>` form as SMA-321. (Done by a human or via the Notion tools after merge — do not leave the published example showing an API that does not exist.)

---

## Self-review

**Spec coverage:**
- Goal 1 (constrain on finalizing turn) → Tasks 6, 7. ✅
- Goal 2 (validate terminal output) → Task 6 (`validate_terminal`). ✅
- Goal 3 (exactly one repair, then `InvalidStructuredOutput { schema_errors, final_text }`) → Task 8. ✅
- Goal 4 (`RunResult<T>` via `collect_typed`) → Task 3. ✅
- D1 (logic in pure `transition`) → Tasks 5–8 (all in `loop_state.rs`). ✅
- D2 (validator-only closure) → Task 2. ✅
- D3 (validation engine) → serde-only; jsonschema dropped (MSRV 1.83 > 1.75) per the spec's degradation clause — recorded in File Structure note. ✅
- D4 (single-layer `strict()` in core, providers call it) → Task 1. ✅
- D5 (separate one-shot budget, `RepairStarted` event) → Tasks 3 (event), 8 (budget). ✅
- D6 (output_type overrides on finalizing turn only) → `constrained_settings` only used in `Finalizing`/`RepairingOutput` paths (Tasks 6–8). ✅
- D7 (two-phase) → Tasks 6 (no-tools), 7 (tools). ✅
- D8 (`collect_typed`; `Runner::run -> RunResult<T>` deferred) → Task 3 + Out-of-scope. ✅
- `core::schema::strict` + OpenAI delegate + facade re-export → Task 1. ✅
- `AgentError` reshape + semver/breaking handling → Task 4. ✅
- Replay persistence (repair message returned on `TransitionOutcome`, driver appends) → Task 5 (field) + Task 8 (use). ✅
- AC#1 (no-tools + tools) → Tasks 6, 7. AC#2 → Task 8. Example → Task 9. ✅
- Notion doc-sync → Post-merge follow-up. ✅

**Placeholder scan:** No `TBD`/`TODO`/"handle edge cases". Two steps (Task 3 ordering note, Task 9 constructor-name verification) flag concrete verification points with explicit fallbacks, not vague gaps.

**Type consistency:** `OutputType { name, schema, validate }` (Task 2) used consistently by `constrained_settings`/`validate_terminal` (Task 6). `TransitionOutcome.conversation_appends` defined (Task 5) and populated (Task 8). `AgentEvent::{RepairStarted, StructuredOutputFailed}` defined (Task 3) and emitted (Tasks 6, 8) and consumed (Task 3 `collect_typed`). `AgentError::InvalidStructuredOutput { schema_errors, final_text }` defined (Task 4), produced (Tasks 6, 8), consumed (Task 3). `constrained_settings`/`validate_terminal`/`flatten_text`/`last_assistant_content`/`repair_message` names stable across tasks. ✅

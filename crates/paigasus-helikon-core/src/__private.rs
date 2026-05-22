//! Implementation details exposed to macro-generated code.
//!
//! **Semver-exempt.** Items in this module are not part of the public API.
//! Only the `#[tool]` and `tools!` macros in `paigasus-helikon-macros`
//! are expected to reference them. Direct use by application code is
//! unsupported and may break without notice.

use std::marker::PhantomData;

// Re-export so generated code can name it absolutely via
// `::paigasus_helikon_core::__private::async_trait::async_trait`.
pub use async_trait;

/// Type-level probe used by `#[tool]` to decide whether `Out: JsonSchema`.
///
/// The macro emits `(&&OutputSchemaProbe::<Out>::NEW).schema()`. Method
/// resolution starts at `&&Probe<Out>`, auto-derefs once to `&Probe<Out>`,
/// and finds the `OutputSchemaProbeSpec::schema` impl iff
/// `Out: JsonSchema`. If the bound holds, the specialized arm wins
/// (fewer deref steps); otherwise resolution falls through to the
/// inherent `fn schema(&self) -> None` fallback.
pub struct OutputSchemaProbe<T>(PhantomData<T>);

impl<T> OutputSchemaProbe<T> {
    /// Construct the probe (used by macro-generated code).
    pub const NEW: Self = Self(PhantomData);
}

/// Trait that carries the specialized arm of the autoref-specialization
/// trick. `OutputSchemaProbeSpec for &OutputSchemaProbe<T>` is one
/// deref step closer than the inherent fallback, so method resolution
/// prefers it when `T: JsonSchema` holds.
pub trait OutputSchemaProbeSpec {
    /// Return the JSON Schema for `T`, or `None` if `T: JsonSchema` does not hold.
    fn schema(&self) -> Option<serde_json::Value>;
}

impl<T: schemars::JsonSchema> OutputSchemaProbeSpec for &OutputSchemaProbe<T> {
    fn schema(&self) -> Option<serde_json::Value> {
        serde_json::to_value(schemars::schema_for!(T)).ok()
    }
}

impl<T> OutputSchemaProbe<T> {
    /// Fallback arm — runs when `T: JsonSchema` does not hold. Returns `None`.
    pub fn schema(&self) -> Option<serde_json::Value> {
        None
    }
}

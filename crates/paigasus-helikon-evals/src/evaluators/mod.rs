//! Built-in [`Evaluator`](crate::Evaluator) implementations.

mod exact_match;
mod json_schema;

pub use exact_match::ExactMatch;
pub use json_schema::JsonSchemaConformance;

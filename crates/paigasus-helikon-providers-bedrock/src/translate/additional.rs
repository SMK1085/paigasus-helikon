//! The `additionalModelRequestFields` document of a request.
//!
//! The Converse API has no field for some Anthropic request options. The
//! provider sends these options in `additionalModelRequestFields`. This
//! module holds the data and builds the JSON value.

use serde_json::{json, Map, Value};

/// A tool choice that the provider sends in `additionalModelRequestFields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AdditionalToolChoice {
    /// `{"type": "none"}`: the model must not call a tool.
    None,
    /// `{"type": "any"}`: the model must call a tool.
    Any,
    /// `{"type": "tool", "name": ...}`: the model must call the named tool.
    Tool(String),
}

/// The fields that the provider sends in `additionalModelRequestFields`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AdditionalFields {
    /// Typed tool entries, in the order that the request lists them.
    pub(crate) tools: Vec<Value>,
    /// The tool choice, if the request needs one in this document.
    pub(crate) tool_choice: Option<AdditionalToolChoice>,
}

impl AdditionalFields {
    /// The JSON object to send, or `None` when no field is set.
    pub(crate) fn to_value(&self) -> Option<Value> {
        let mut fields = Map::new();
        if !self.tools.is_empty() {
            fields.insert("tools".to_owned(), Value::Array(self.tools.clone()));
        }
        if let Some(choice) = &self.tool_choice {
            let value = match choice {
                AdditionalToolChoice::None => json!({"type": "none"}),
                AdditionalToolChoice::Any => json!({"type": "any"}),
                AdditionalToolChoice::Tool(name) => json!({"type": "tool", "name": name}),
            };
            fields.insert("tool_choice".to_owned(), value);
        }
        (!fields.is_empty()).then_some(Value::Object(fields))
    }

    /// `true` when the tool choice forces a tool call (`any` or `tool`).
    pub(crate) fn is_forced(&self) -> bool {
        matches!(
            self.tool_choice,
            Some(AdditionalToolChoice::Any | AdditionalToolChoice::Tool(_))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_fields_give_no_value() {
        assert_eq!(AdditionalFields::default().to_value(), None);
    }

    #[test]
    fn choices_have_the_anthropic_shape() {
        let f = |c| {
            AdditionalFields {
                tools: vec![],
                tool_choice: Some(c),
            }
            .to_value()
            .unwrap()
        };
        assert_eq!(
            f(AdditionalToolChoice::None),
            json!({"tool_choice": {"type": "none"}})
        );
        assert_eq!(
            f(AdditionalToolChoice::Any),
            json!({"tool_choice": {"type": "any"}})
        );
        assert_eq!(
            f(AdditionalToolChoice::Tool("memory".into())),
            json!({"tool_choice": {"type": "tool", "name": "memory"}})
        );
    }

    #[test]
    fn tools_are_sent_in_order() {
        let v = AdditionalFields {
            tools: vec![json!({"a":1}), json!({"b":2})],
            tool_choice: None,
        }
        .to_value();
        assert_eq!(v, Some(json!({"tools": [{"a":1}, {"b":2}]})));
    }

    #[test]
    fn only_any_and_tool_are_forced() {
        let f = |c| {
            AdditionalFields {
                tools: vec![],
                tool_choice: c,
            }
            .is_forced()
        };
        assert!(!f(None));
        assert!(!f(Some(AdditionalToolChoice::None)));
        assert!(f(Some(AdditionalToolChoice::Any)));
        assert!(f(Some(AdditionalToolChoice::Tool("x".into()))));
    }
}

//! Per-handle policy for the forced tool choice and the structured-output path.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::family::{rejects_forced_tool_choice, ModelFamily};

/// The wire path that carries a structured-output or forced-tool request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputPath {
    /// Use a synthetic forced tool.
    ForcedTool,
    /// Use the native structured-output field.
    Native,
}

/// A notice that the native path changes the request. Each kind is logged once per handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeNotice {
    /// `ToolChoice::Required` is omitted.
    RequiredOmitted,
    /// `ToolChoice::Tool` is omitted.
    ToolOmitted,
    /// `ResponseFormat::JsonObject` degrades to text.
    JsonObjectAsText,
}

/// State that selects the output path for one model handle.
#[derive(Debug)]
pub(crate) struct ForcedChoicePolicy {
    rejects_static: bool,
    learned: AtomicBool,
    family: ModelFamily,
    warned: [AtomicBool; 3],
}

impl ForcedChoicePolicy {
    /// Make a policy. The static rule comes from the model id.
    pub(crate) fn new(model_id: &str, family: ModelFamily) -> Self {
        Self::with_rule(rejects_forced_tool_choice(model_id), family)
    }

    /// Make a policy with no static rule.
    #[cfg(test)]
    pub(crate) fn without_static_rule(family: ModelFamily) -> Self {
        Self::with_rule(false, family)
    }

    fn with_rule(rejects_static: bool, family: ModelFamily) -> Self {
        Self {
            rejects_static,
            learned: AtomicBool::new(false),
            family,
            warned: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
        }
    }

    /// The model family of this handle.
    // Used from Task 4 (build_request path selection).
    #[allow(dead_code)]
    pub(crate) fn family(&self) -> ModelFamily {
        self.family
    }

    /// The output path for the next request.
    pub(crate) fn path(&self) -> OutputPath {
        if self.rejects_static || self.learned.load(Ordering::SeqCst) {
            OutputPath::Native
        } else {
            OutputPath::ForcedTool
        }
    }

    /// Record that the service rejected a forced tool choice. Returns `true` only for the first call.
    // Used from Task 5 (send fallback).
    #[allow(dead_code)]
    pub(crate) fn record_rejection(&self) -> bool {
        !self.learned.swap(true, Ordering::SeqCst)
    }

    /// Returns `true` only the first time for each notice kind.
    pub(crate) fn first_notice(&self, n: NativeNotice) -> bool {
        !self.warned[n as usize].swap(true, Ordering::SeqCst)
    }

    /// Log the notice once per handle.
    pub(crate) fn note(&self, n: NativeNotice, model_id: &str) {
        if !self.first_notice(n) {
            return;
        }
        let text = match n {
            NativeNotice::RequiredOmitted => {
                "ToolChoice::Required is omitted: this model rejects a forced tool choice"
            }
            NativeNotice::ToolOmitted => {
                "ToolChoice::Tool is omitted: this model rejects a forced tool choice"
            }
            NativeNotice::JsonObjectAsText => {
                "ResponseFormat::JsonObject degrades to text: native output cannot express a free-form object"
            }
        };
        tracing::warn!(target: "paigasus::bedrock::translate", model_id = %model_id, "{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ModelFamily::Anthropic;

    #[test]
    fn path_for_each_combination() {
        assert_eq!(
            ForcedChoicePolicy::new("global.anthropic.claude-sonnet-5-5", Anthropic).path(),
            OutputPath::Native
        );
        assert_eq!(
            ForcedChoicePolicy::new("global.anthropic.claude-sonnet-5", Anthropic).path(),
            OutputPath::ForcedTool
        );
        let p = ForcedChoicePolicy::without_static_rule(Anthropic);
        assert_eq!(p.path(), OutputPath::ForcedTool);
        assert!(p.record_rejection());
        assert_eq!(p.path(), OutputPath::Native);
        let both = ForcedChoicePolicy::new("us.anthropic.claude-opus-5-5", Anthropic);
        both.record_rejection();
        assert_eq!(both.path(), OutputPath::Native);
    }

    #[test]
    fn record_rejection_is_true_only_once_across_threads() {
        let p = std::sync::Arc::new(ForcedChoicePolicy::without_static_rule(Anthropic));
        let firsts: usize = (0..8)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || p.record_rejection())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap() as usize)
            .sum();
        assert_eq!(firsts, 1);
    }

    #[test]
    fn first_notice_is_true_once_per_kind() {
        let p = ForcedChoicePolicy::without_static_rule(Anthropic);
        assert!(p.first_notice(NativeNotice::RequiredOmitted));
        assert!(!p.first_notice(NativeNotice::RequiredOmitted));
        assert!(p.first_notice(NativeNotice::ToolOmitted));
        assert!(p.first_notice(NativeNotice::JsonObjectAsText));
    }
}

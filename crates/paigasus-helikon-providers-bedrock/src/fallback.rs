//! The single send fallback for a forced tool choice that the service rejects.
//!
//! An Anthropic model on Bedrock can reject a forced tool choice
//! (`toolChoice` `any` or `tool`) with a `ValidationException`. When the
//! first request carried a forced choice and the service rejects it before
//! the stream starts, the provider sends one native request, which is a
//! different request, on the structured-output path. The handle then records the rejection, and
//! all later requests on the handle use the native path at once.
//!
//! This fallback does not break ADR-10 (*No silent auto-retry inside the
//! loop*), for four reasons. The fallback sends a *different* request, not
//! the same request again. It occurs at most once for each request. It is
//! logged. It never follows a transient error (throttling, a 5xx error or a
//! transport error): those errors go to the caller unchanged.
//!
//! The match on the error text is a weak contract. If Bedrock changes the
//! text, the result is the same `ValidationException` as before this
//! fallback existed. The static model-id rule still sends the known models
//! on the native path from the first request.

use std::future::Future;

use paigasus_helikon_core::ModelError;

use crate::error::SendFailure;
use crate::family::ModelFamily;
use crate::policy::{ForcedChoicePolicy, OutputPath};
use crate::translate::PreparedConverse;

/// The output of a send, with the `synthesizing` value of the request that
/// was actually sent.
#[derive(Debug)]
pub(crate) struct Sent<T> {
    /// The output of the send.
    pub(crate) output: T,
    /// `synthesizing` of the request that was sent.
    pub(crate) synthesizing: bool,
    /// `true` when the request that was sent used native structured output.
    pub(crate) native: bool,
}

/// Send `first`. If the service rejects its forced tool choice, send the
/// request again on the native path, one time.
///
/// The fallback occurs only when all of these conditions are true:
/// the model family is Anthropic, `first` carried a forced tool choice, and
/// the failure is a rejection of that choice. `rebuild` makes the native
/// request. An error from the second send goes to the caller with no
/// further fallback.
pub(crate) async fn send_with_fallback<T, B, S, Fut>(
    policy: &ForcedChoicePolicy,
    model_id: &str,
    first: PreparedConverse,
    rebuild: B,
    mut send: S,
) -> Result<Sent<T>, ModelError>
where
    B: FnOnce(OutputPath) -> Result<PreparedConverse, ModelError>,
    S: FnMut(PreparedConverse) -> Fut,
    Fut: Future<Output = Result<T, SendFailure>>,
{
    for n in &first.notices {
        policy.note(*n, model_id);
    }
    let synthesizing = first.synthesizing;
    let native = first.output_config.is_some();
    let first_was_forced = first.has_forced_tool_choice();

    let failure = match send(first).await {
        Ok(output) => {
            return Ok(Sent {
                output,
                synthesizing,
                native,
            })
        }
        Err(f) => f,
    };

    if !(policy.family() == ModelFamily::Anthropic
        && first_was_forced
        && failure.forced_choice_rejected)
    {
        return Err(failure.error);
    }

    if policy.record_rejection() {
        tracing::warn!(
            target: "paigasus::bedrock::model",
            model_id = %model_id,
            "the model rejected a forced tool choice; native structured output is now in use for this handle"
        );
    }

    let second = rebuild(OutputPath::Native)?;
    for n in &second.notices {
        policy.note(*n, model_id);
    }
    let synthesizing = second.synthesizing;
    let native = second.output_config.is_some();
    match send(second).await {
        Ok(output) => Ok(Sent {
            output,
            synthesizing,
            native,
        }),
        Err(f) => Err(f.error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use paigasus_helikon_core::{ContentPart, Item, ModelError, ModelRequest, ResponseFormat};
    use serde_json::{json, Value};

    use crate::builder::{offline_config, Config};
    use crate::error::SendFailure;
    use crate::family::ModelFamily;
    use crate::policy::{ForcedChoicePolicy, OutputPath};
    use crate::translate::tools::SYNTHESIZED_TOOL_NAME;
    use crate::translate::{build_request, to_wire_json};

    const CLAUDE: &str = "anthropic.claude-3-5-sonnet-20241022-v2:0";
    const MISTRAL: &str = "mistral.mistral-large-2407-v1:0";

    fn rejected() -> SendFailure {
        SendFailure {
            forced_choice_rejected: true,
            error: ModelError::Other(anyhow::anyhow!(
                "bedrock ValidationException: This model doesn't support the toolConfig.toolChoice.any field."
            )),
        }
    }

    fn transaction_request() -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "Coffee, 4.50 EUR".to_owned(),
            }],
        }];
        req.model_settings.response_format = Some(ResponseFormat::JsonSchema {
            name: "Transaction".to_owned(),
            schema: json!({
                "type": "object",
                "properties": {
                    "amount": {"type": "number"},
                    "currency": {"type": "string"},
                },
                "required": ["amount", "currency"]
            }),
            strict: false,
        });
        req
    }

    fn text_request() -> ModelRequest {
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text {
                text: "Hello".to_owned(),
            }],
        }];
        req
    }

    /// Run `send_with_fallback` with a scripted fake send. Returns the result
    /// and the wire projection of each request that the fake received.
    fn drive(
        policy: &ForcedChoicePolicy,
        cfg: &Config,
        req: &ModelRequest,
        script: Vec<Result<(), SendFailure>>,
    ) -> (Result<Sent<()>, ModelError>, Vec<Value>) {
        let first = build_request(cfg, req, policy.path()).expect("first request");
        let sent: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let send = {
            let sent = sent.clone();
            let script = script.clone();
            move |p: crate::translate::PreparedConverse| {
                sent.lock().unwrap().push(to_wire_json(&p));
                let next = script
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("a send that the script does not cover");
                async move { next }
            }
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let result = rt.block_on(send_with_fallback(
            policy,
            &cfg.model_id,
            first,
            |path| build_request(cfg, req, path),
            send,
        ));
        let wires = sent.lock().unwrap().clone();
        (result, wires)
    }

    fn assert_forced(wire: &Value) {
        assert_eq!(
            wire["toolConfig"]["toolChoice"],
            json!({"tool": {"name": SYNTHESIZED_TOOL_NAME}}),
            "{wire:#}"
        );
    }

    fn assert_native(wire: &Value) {
        assert!(!wire["outputConfig"].is_null(), "{wire:#}");
        assert!(wire["toolConfig"].is_null(), "{wire:#}");
    }

    #[test]
    fn rejection_falls_back_once_to_native() {
        let cfg = offline_config(CLAUDE);
        let policy = ForcedChoicePolicy::without_static_rule(ModelFamily::Anthropic);
        let (res, wires) = drive(
            &policy,
            &cfg,
            &transaction_request(),
            vec![Err(rejected()), Ok(())],
        );
        assert_eq!(wires.len(), 2);
        assert_forced(&wires[0]);
        assert_native(&wires[1]);
        assert_eq!(policy.path(), OutputPath::Native);
        assert!(res.is_ok());
    }

    #[test]
    fn second_request_on_same_handle_goes_native_at_once() {
        let cfg = offline_config(CLAUDE);
        let policy = ForcedChoicePolicy::without_static_rule(ModelFamily::Anthropic);
        let req = transaction_request();
        let (res, _) = drive(&policy, &cfg, &req, vec![Err(rejected()), Ok(())]);
        assert!(res.is_ok());
        let (res, wires) = drive(&policy, &cfg, &req, vec![Ok(())]);
        assert!(res.is_ok());
        assert_eq!(wires.len(), 1);
        assert_native(&wires[0]);
    }

    #[test]
    fn non_anthropic_family_does_not_fall_back() {
        let cfg = offline_config(MISTRAL);
        let policy = ForcedChoicePolicy::without_static_rule(ModelFamily::Mistral);
        let (res, wires) = drive(&policy, &cfg, &transaction_request(), vec![Err(rejected())]);
        assert_eq!(wires.len(), 1);
        assert_forced(&wires[0]);
        assert!(res.is_err());
        assert_eq!(policy.path(), OutputPath::ForcedTool);
    }

    #[test]
    fn other_errors_do_not_fall_back() {
        let cfg = offline_config(CLAUDE);
        let policy = ForcedChoicePolicy::without_static_rule(ModelFamily::Anthropic);
        let (res, wires) = drive(
            &policy,
            &cfg,
            &transaction_request(),
            vec![Err(SendFailure {
                forced_choice_rejected: false,
                error: ModelError::RateLimited {
                    retry_after_ms: Some(1000),
                },
            })],
        );
        assert_eq!(wires.len(), 1);
        assert!(matches!(
            res,
            Err(ModelError::RateLimited {
                retry_after_ms: Some(1000)
            })
        ));
        assert_eq!(policy.path(), OutputPath::ForcedTool);
    }

    #[test]
    fn second_rejection_is_returned() {
        let cfg = offline_config(CLAUDE);
        let policy = ForcedChoicePolicy::without_static_rule(ModelFamily::Anthropic);
        let (res, wires) = drive(
            &policy,
            &cfg,
            &transaction_request(),
            vec![Err(rejected()), Err(rejected())],
        );
        assert_eq!(wires.len(), 2);
        assert!(matches!(res, Err(ModelError::Other(_))));
        assert_eq!(policy.path(), OutputPath::Native);
    }

    #[test]
    fn request_without_forced_choice_does_not_fall_back() {
        let cfg = offline_config(CLAUDE);
        let policy = ForcedChoicePolicy::without_static_rule(ModelFamily::Anthropic);
        let (res, wires) = drive(&policy, &cfg, &text_request(), vec![Err(rejected())]);
        assert_eq!(wires.len(), 1);
        assert!(res.is_err());
        assert_eq!(policy.path(), OutputPath::ForcedTool);
    }

    #[test]
    fn fallback_returns_synthesizing_of_the_sent_request() {
        let cfg = offline_config(CLAUDE);
        let policy = ForcedChoicePolicy::without_static_rule(ModelFamily::Anthropic);
        let (res, wires) = drive(
            &policy,
            &cfg,
            &transaction_request(),
            vec![Err(rejected()), Ok(())],
        );
        assert_eq!(wires[0]["synthesizing"], json!(true));
        let sent = res.expect("the fallback send succeeds");
        assert!(!sent.synthesizing);
    }
}

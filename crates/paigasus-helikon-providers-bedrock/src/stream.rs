//! `StreamTranslator` — Bedrock `ConverseStreamOutput` events → `ModelEvent`.
//!
//! ## Usage / Finish ordering
//!
//! The Bedrock Converse API emits events in this order:
//!
//! ```text
//! MessageStart
//! ContentBlockStart | ContentBlockDelta | ContentBlockStop  (interleaved)
//! MessageStop         ← stop_reason lives here
//! Metadata            ← usage lives here (AFTER MessageStop)
//! ```
//!
//! Because `ModelEvent::Usage` must precede `ModelEvent::Finish` (per the
//! `paigasus_helikon_core::Model::invoke` ordering contract), and Bedrock
//! emits `Metadata` **after** `MessageStop`, this translator buffers the
//! stop reason when `MessageStop` arrives and emits nothing until `Metadata`
//! arrives — at that point it emits `Usage` immediately followed by `Finish`.
//!
//! If (contrary to the typical wire order) `Metadata` arrives **before**
//! `MessageStop`, `Usage` is emitted immediately on `Metadata` and `Finish`
//! is emitted immediately on `MessageStop`, which also satisfies the contract.

use std::collections::HashMap;

use aws_sdk_bedrockruntime::types::{
    ContentBlockDelta, ContentBlockStart, ConverseStreamOutput, ReasoningContentBlockDelta,
    StopReason,
};
use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

use crate::translate::tools::SYNTHESIZED_TOOL_NAME;

// ── internal block state ──────────────────────────────────────────────────────

/// Per-content-block state keyed by `content_block_index`.
///
/// Reasoning content blocks are identified purely by the
/// `ContentBlockDelta::ReasoningContent` delta variant; no separate block-state
/// entry is needed for them.
#[derive(Debug)]
enum BlockState {
    Text,
    ToolUse {
        call_id: String,
        name: String,
        /// `true` after the first `ToolCallDelta` has been emitted (name
        /// appears only in that first delta; subsequent deltas set it to
        /// `None`).
        name_emitted: bool,
    },
}

// ── public translator ─────────────────────────────────────────────────────────

/// State machine that maps Bedrock `ConverseStreamOutput` events to
/// `paigasus_helikon_core::ModelEvent`s.
///
/// Call [`StreamTranslator::consume`] once per SDK event.  The translator is
/// pure (no I/O) and is intended to be driven from an `async_stream!` wrapper
/// that feeds it chunks from `converse_stream`.
///
/// ## Synthesizing mode
///
/// When `synthesizing = true` the model was asked to fill a JSON schema via a
/// forced-tool call (`__paigasus_structured_output__`).  In that mode the
/// synthesized tool's input deltas are re-emitted as `TokenDelta`s and the
/// terminal `tool_use` stop reason is rewritten to `Stop`.  If a *real* tool
/// fires alongside the synthesized one, `ModelError::Other` is returned
/// (protocol violation).
#[derive(Debug, Default)]
pub struct StreamTranslator {
    /// `content_block_index` → per-block state.
    blocks: HashMap<i32, BlockState>,
    /// Whether we are in structured-output synthesis mode.
    synthesizing: bool,
    /// The content-block index of the synthesized tool's block (if any).
    synthesized_block_index: Option<i32>,
    /// `true` if at least one *real* (non-synthesized) tool block started.
    real_tool_fired: bool,
    /// Buffered stop reason from `MessageStop`; held until `Metadata` arrives
    /// so that `Usage` can be emitted first.
    pending_stop_reason: Option<StopReason>,
    /// `true` once a `Metadata` event has been processed.
    metadata_seen: bool,
}

impl StreamTranslator {
    /// Create a new translator.
    ///
    /// Set `synthesizing` to `true` when the request was built with
    /// structured-output synthesis (forced `__paigasus_structured_output__`
    /// tool call).
    pub fn new(synthesizing: bool) -> Self {
        Self {
            synthesizing,
            ..Default::default()
        }
    }

    /// Consume one `ConverseStreamOutput` event and return zero or more
    /// `ModelEvent`s (or `ModelError`s) produced by it.
    pub fn consume(&mut self, ev: ConverseStreamOutput) -> Vec<Result<ModelEvent, ModelError>> {
        let mut out: Vec<Result<ModelEvent, ModelError>> = Vec::new();

        match ev {
            // ── events that produce no output ─────────────────────────────────
            ConverseStreamOutput::MessageStart(_) => {}
            ConverseStreamOutput::ContentBlockStop(_) => {}

            // ── content block start ───────────────────────────────────────────
            ConverseStreamOutput::ContentBlockStart(e) => {
                let idx = e.content_block_index();
                match e.start() {
                    Some(ContentBlockStart::ToolUse(tu)) => {
                        let call_id = tu.tool_use_id().to_owned();
                        let name = tu.name().to_owned();

                        if self.synthesizing && name == SYNTHESIZED_TOOL_NAME {
                            self.synthesized_block_index = Some(idx);
                        } else {
                            self.real_tool_fired = true;
                            // Emit the name-carrying delta immediately on block
                            // start (mirrors the Anthropic provider's behaviour
                            // of emitting name on the first input-json delta).
                            // We *don't* emit here — we wait for the first
                            // tool-input delta so the call_id→state map is
                            // consistent.
                        }
                        self.blocks.insert(
                            idx,
                            BlockState::ToolUse {
                                call_id,
                                name,
                                name_emitted: false,
                            },
                        );
                    }
                    Some(ContentBlockStart::Image(_) | ContentBlockStart::ToolResult(_)) => {
                        // Not a tool we translate; record nothing.
                    }
                    Some(_) | None => {
                        // Text or unknown start: record as text block.
                        self.blocks.insert(idx, BlockState::Text);
                    }
                }
            }

            // ── content block delta ───────────────────────────────────────────
            ConverseStreamOutput::ContentBlockDelta(e) => {
                let idx = e.content_block_index();
                match e.delta() {
                    Some(ContentBlockDelta::Text(text)) => {
                        out.push(Ok(ModelEvent::TokenDelta { text: text.clone() }));
                    }
                    Some(ContentBlockDelta::ToolUse(tu_delta)) => {
                        let input = tu_delta.input().to_owned();
                        let is_synth = Some(idx) == self.synthesized_block_index;

                        if is_synth {
                            // Remap synthesized tool input to TokenDelta.
                            out.push(Ok(ModelEvent::TokenDelta { text: input }));
                        } else if let Some(BlockState::ToolUse {
                            call_id,
                            name,
                            name_emitted,
                        }) = self.blocks.get_mut(&idx)
                        {
                            let emit_name = if *name_emitted {
                                None
                            } else {
                                *name_emitted = true;
                                Some(name.clone())
                            };
                            out.push(Ok(ModelEvent::ToolCallDelta {
                                call_id: call_id.clone(),
                                name: emit_name,
                                args_delta: input,
                            }));
                        }
                        // If no block state (shouldn't happen for well-formed
                        // streams), silently drop rather than panic.
                    }
                    Some(ContentBlockDelta::ReasoningContent(rc)) => match rc {
                        ReasoningContentBlockDelta::Text(text) => {
                            out.push(Ok(ModelEvent::ReasoningDelta { text: text.clone() }));
                        }
                        ReasoningContentBlockDelta::Signature(_) => {
                            // Signature is a round-trip token for multi-turn
                            // extended-thinking; we drop it here (not yet
                            // plumbed through the session layer).
                            tracing::debug!(
                                target: "paigasus::bedrock::stream",
                                "reasoning signature delta dropped (round-trip not yet supported)",
                            );
                        }
                        ReasoningContentBlockDelta::RedactedContent(_) => {
                            // Encrypted reasoning blob — provider-opaque, drop.
                            tracing::debug!(
                                target: "paigasus::bedrock::stream",
                                "reasoning redacted-content delta dropped",
                            );
                        }
                        _ => {
                            // Forward-compat: ignore unknown reasoning variants.
                        }
                    },
                    // Image, ToolResult, Citation, Unknown deltas — drop.
                    Some(_) | None => {}
                }
            }

            // ── message stop (stop_reason) ────────────────────────────────────
            ConverseStreamOutput::MessageStop(e) => {
                let stop_reason = e.stop_reason().clone();
                if self.metadata_seen {
                    // Metadata already arrived; emit Finish now.
                    out.extend(self.finish_events(stop_reason));
                } else {
                    // Buffer; Metadata will trigger Usage + Finish.
                    self.pending_stop_reason = Some(stop_reason);
                }
            }

            // ── metadata (usage) ──────────────────────────────────────────────
            ConverseStreamOutput::Metadata(e) => {
                self.metadata_seen = true;
                if let Some(usage) = e.usage() {
                    out.push(Ok(ModelEvent::Usage {
                        input_tokens: usage.input_tokens() as u32,
                        output_tokens: usage.output_tokens() as u32,
                        cached_input_tokens: usage.cache_read_input_tokens().map(|n| n as u32),
                        reasoning_tokens: None,
                    }));
                }
                // If a buffered stop reason is waiting, emit Finish now.
                if let Some(reason) = self.pending_stop_reason.take() {
                    out.extend(self.finish_events(reason));
                }
            }

            // Forward-compat catch-all: Unknown and any future enum variants
            // added by the SDK are silently ignored.
            _ => {}
        }

        out
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Translate a `StopReason` into the terminal event(s).
    ///
    /// Returns a single `Ok(Finish{..})` in normal cases, or a single
    /// `Err(ModelError::Other)` when both a real and the synthesized tool
    /// fired in synthesis mode.
    fn finish_events(&self, reason: StopReason) -> Vec<Result<ModelEvent, ModelError>> {
        let finish = match reason {
            StopReason::EndTurn | StopReason::StopSequence => Ok(ModelEvent::Finish {
                reason: FinishReason::Stop,
            }),
            StopReason::ToolUse => {
                if self.synthesizing {
                    if self.real_tool_fired {
                        // Both tools fired — protocol violation.
                        Err(ModelError::Other(anyhow::anyhow!(
                            "bedrock stream: structured-output synthesis: \
                             model fired both a real tool and the synthesized \
                             output tool '__paigasus_structured_output__'"
                        )))
                    } else {
                        // Only synthesized tool fired — rewrite to Stop.
                        Ok(ModelEvent::Finish {
                            reason: FinishReason::Stop,
                        })
                    }
                } else {
                    Ok(ModelEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    })
                }
            }
            StopReason::MaxTokens => Ok(ModelEvent::Finish {
                reason: FinishReason::Length,
            }),
            StopReason::GuardrailIntervened | StopReason::ContentFiltered => {
                Ok(ModelEvent::Finish {
                    reason: FinishReason::ContentFilter,
                })
            }
            other => Ok(ModelEvent::Finish {
                reason: FinishReason::Other(other.as_str().to_owned()),
            }),
        };
        vec![finish]
    }
}

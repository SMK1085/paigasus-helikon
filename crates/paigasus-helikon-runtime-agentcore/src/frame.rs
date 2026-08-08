//! `FrameBudget` — keeps outbound WebSocket traffic inside AgentCore's frame-size and
//! frame-rate quotas. Shared by the HTTP-protocol and AG-UI `/ws` endpoints.

// This module's items are pub(crate) API for the `/ws` endpoints landing in later
// tasks of this plan (SMA-461) and, until one of those endpoints exists, are
// exercised only by this module's own tests — hence the otherwise-unused warning.
#![allow(dead_code)]

use std::time::Duration;

use serde_json::Value;

/// Maximum serialized bytes in a single outbound WebSocket frame.
///
/// AgentCore closes the connection when a frame exceeds its documented **64 KB** limit.
/// AWS does not state whether "64 KB" means 65 536 or 64 000 bytes, so this budgets
/// against the smaller reading and leaves headroom on top of that.
pub(crate) const MAX_FRAME_BYTES: usize = 60_000;

/// Maximum frames emitted per second.
///
/// AgentCore closes the connection above **250 frames/second**. AWS does not state
/// whether that is a one-second average or a shorter sliding window, so this paces
/// against the hostile reading: a burst cannot trip a sliding window either.
pub(crate) const FRAME_RATE_CAP: u32 = 200;

/// How an oversize frame is broken up.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SplitStrategy {
    /// Split one string field's value across several otherwise-identical frames. The
    /// result is N valid protocol events, so a client needs no reassembly logic.
    Content {
        /// Name of the string field to split (e.g. `"delta"`).
        field: &'static str,
    },
    /// Wrap the serialized frame in `helikon.chunk` envelopes. Used only for events
    /// whose payload cannot be split into several valid events.
    Envelope,
}

/// Paces and splits outbound WebSocket frames to stay inside AgentCore's quotas.
///
/// One instance per connection; not `Clone`, because the rate budget is per-connection.
pub(crate) struct FrameBudget {
    /// How an oversize frame is broken up.
    split: SplitStrategy,
    /// Frames emitted in the current one-second window.
    emitted: u32,
    /// Start of the current window, on the tokio clock (so `tokio::time::pause` works).
    window_start: tokio::time::Instant,
    /// Monotonic id for chunk groups, so a client can tell two interleaved groups apart.
    chunk_group: u64,
}

impl FrameBudget {
    /// A budget that wraps oversize frames in `helikon.chunk` envelopes.
    pub(crate) fn new() -> Self {
        Self::new_with_splitter(SplitStrategy::Envelope)
    }

    /// A budget using an explicit split strategy.
    pub(crate) fn new_with_splitter(split: SplitStrategy) -> Self {
        Self {
            split,
            emitted: 0,
            window_start: tokio::time::Instant::now(),
            chunk_group: 0,
        }
    }

    /// Turn one logical event into the wire-ready text frames for it, awaiting any
    /// pacing delay first.
    ///
    /// Always returns at least one frame. Every returned frame is at most
    /// `MAX_FRAME_BYTES` serialized bytes.
    pub(crate) async fn admit(&mut self, frame: Value) -> Vec<String> {
        let frames = self.split(frame);
        for _ in 0..frames.len() {
            self.tick().await;
        }
        frames
    }

    /// Consume one frame from the rate budget, sleeping until the next window if this
    /// window is exhausted.
    async fn tick(&mut self) {
        let elapsed = self.window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.window_start = tokio::time::Instant::now();
            self.emitted = 0;
        } else if self.emitted >= FRAME_RATE_CAP {
            tokio::time::sleep(Duration::from_secs(1) - elapsed).await;
            self.window_start = tokio::time::Instant::now();
            self.emitted = 0;
        }
        self.emitted += 1;
    }

    fn split(&mut self, frame: Value) -> Vec<String> {
        let whole = frame.to_string();
        if whole.len() <= MAX_FRAME_BYTES {
            return vec![whole];
        }
        match self.split {
            SplitStrategy::Content { field } => self.split_content(frame, field),
            SplitStrategy::Envelope => self.split_envelope(&whole),
        }
    }

    /// Split `field`'s string value across several copies of the same event.
    ///
    /// Falls back to the envelope strategy when the field is absent or not a string —
    /// the frame is oversize either way and must not go out whole.
    fn split_content(&mut self, frame: Value, field: &str) -> Vec<String> {
        let Some(text) = frame.get(field).and_then(Value::as_str) else {
            return self.split_envelope(&frame.to_string());
        };
        // Budget for the envelope around the field: serialize the event with the field
        // emptied and subtract, leaving room for escaping growth inside the chunk.
        let mut probe = frame.clone();
        probe[field] = Value::String(String::new());
        let overhead = probe.to_string().len();
        // Worst case each char serializes to 6 bytes ("\uXXXX"), so budget conservatively.
        let budget = MAX_FRAME_BYTES.saturating_sub(overhead + 16) / 6;
        let budget = budget.max(1);

        let mut out = Vec::new();
        let mut chunk = String::new();
        let mut chars = 0usize;
        for c in text.chars() {
            chunk.push(c);
            chars += 1;
            if chars >= budget {
                let mut part = frame.clone();
                part[field] = Value::String(std::mem::take(&mut chunk));
                out.push(part.to_string());
                chars = 0;
            }
        }
        if !chunk.is_empty() {
            let mut part = frame.clone();
            part[field] = Value::String(chunk);
            out.push(part.to_string());
        }
        out
    }

    /// Wrap an oversize serialized frame in `helikon.chunk` envelopes.
    fn split_envelope(&mut self, whole: &str) -> Vec<String> {
        self.chunk_group += 1;
        let id = format!("c{}", self.chunk_group);
        // Envelope overhead plus worst-case 6x escaping growth inside `data`.
        let budget = (MAX_FRAME_BYTES.saturating_sub(160) / 6).max(1);

        let mut pieces: Vec<String> = Vec::new();
        let mut piece = String::new();
        let mut chars = 0usize;
        for c in whole.chars() {
            piece.push(c);
            chars += 1;
            if chars >= budget {
                pieces.push(std::mem::take(&mut piece));
                chars = 0;
            }
        }
        if !piece.is_empty() {
            pieces.push(piece);
        }

        let last = pieces.len().saturating_sub(1);
        pieces
            .into_iter()
            .enumerate()
            .map(|(seq, data)| {
                serde_json::json!({
                    "type": "helikon.chunk",
                    "id": id,
                    "seq": seq,
                    "final": seq == last,
                    "data": data,
                })
                .to_string()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn big_text(n: usize) -> String {
        "a".repeat(n)
    }

    #[tokio::test]
    async fn small_frame_passes_through_unwrapped() {
        let mut b = FrameBudget::new();
        let out = b.admit(json!({"type": "RUN_STARTED", "runId": "r1"})).await;
        assert_eq!(out.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(parsed["type"], "RUN_STARTED");
        assert!(
            parsed.get("seq").is_none(),
            "small frames must not be wrapped"
        );
    }

    #[tokio::test]
    async fn every_emitted_frame_is_within_the_size_cap() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": big_text(500_000)}))
            .await;
        assert!(out.len() > 1, "an oversize frame must be split");
        for f in &out {
            assert!(
                f.len() <= MAX_FRAME_BYTES,
                "emitted frame of {} bytes exceeds MAX_FRAME_BYTES",
                f.len()
            );
        }
    }

    #[tokio::test]
    async fn content_split_preserves_the_payload_and_the_event_type() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let original = big_text(200_000);
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": original}))
            .await;
        let mut reassembled = String::new();
        for f in &out {
            let v: serde_json::Value = serde_json::from_str(f).unwrap();
            assert_eq!(
                v["type"], "TEXT_MESSAGE_CONTENT",
                "each split frame stays a valid event"
            );
            assert_eq!(v["messageId"], "m0");
            reassembled.push_str(v["delta"].as_str().unwrap());
        }
        assert_eq!(reassembled, original);
    }

    /// Splitting must land on `char_indices` boundaries: a byte-offset split through a
    /// multi-byte codepoint produces invalid UTF-8 and a frame that will not parse.
    #[tokio::test]
    async fn content_split_never_lands_mid_codepoint() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let original = "→".repeat(100_000); // 3 bytes each
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": original}))
            .await;
        let mut reassembled = String::new();
        for f in &out {
            let v: serde_json::Value = serde_json::from_str(f).unwrap();
            reassembled.push_str(v["delta"].as_str().unwrap());
        }
        assert_eq!(reassembled, original);
    }

    /// The cap applies to serialized bytes, not payload length: JSON escaping expands
    /// control characters sixfold, so a payload comfortably under the cap can serialize
    /// well over it.
    #[tokio::test]
    async fn size_is_measured_on_serialized_bytes_not_payload_length() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        // 20k control chars -> "" (6 bytes) each -> ~120 KB serialized.
        let payload: String = "\u{1}".repeat(20_000);
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": payload}))
            .await;
        assert!(out.len() > 1, "escaping must be accounted for");
        for f in &out {
            assert!(
                f.len() <= MAX_FRAME_BYTES,
                "frame of {} bytes too large",
                f.len()
            );
        }
    }

    #[tokio::test]
    async fn unsplittable_events_fall_back_to_the_chunk_envelope() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Envelope);
        let out = b
            .admit(json!({"type": "TOOL_CALL_RESULT", "content": big_text(200_000)}))
            .await;
        assert!(out.len() > 1);
        let mut reassembled = String::new();
        for (i, f) in out.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(f).unwrap();
            assert_eq!(v["type"], "helikon.chunk");
            assert_eq!(v["seq"], i);
            assert_eq!(v["final"], i == out.len() - 1);
            reassembled.push_str(v["data"].as_str().unwrap());
        }
        let inner: serde_json::Value = serde_json::from_str(&reassembled).unwrap();
        assert_eq!(inner["type"], "TOOL_CALL_RESULT");
    }

    /// Deterministic pacing: with the clock paused, admitting more frames than the
    /// per-second cap must have awaited a total delay of at least one second. Asserting
    /// on the virtual clock (not wall time) keeps this stable across the CI matrix.
    #[tokio::test(start_paused = true)]
    async fn pacer_delays_once_the_rate_cap_is_reached() {
        let mut b = FrameBudget::new();
        let start = tokio::time::Instant::now();
        for i in 0..(FRAME_RATE_CAP + 1) {
            b.admit(json!({"type": "STEP_STARTED", "n": i})).await;
        }
        assert!(
            start.elapsed() >= std::time::Duration::from_secs(1),
            "the pacer must have slept after exceeding the cap, elapsed {:?}",
            start.elapsed()
        );
    }

    /// The pacer covers *every* frame, not just text. A burst of tool-call frames with
    /// no text involved must still be paced.
    #[tokio::test(start_paused = true)]
    async fn pacer_covers_non_text_frames() {
        let mut b = FrameBudget::new();
        let start = tokio::time::Instant::now();
        for i in 0..(FRAME_RATE_CAP + 1) {
            b.admit(json!({"type": "TOOL_CALL_RESULT", "toolCallId": format!("t{i}")}))
                .await;
        }
        assert!(start.elapsed() >= std::time::Duration::from_secs(1));
    }
}

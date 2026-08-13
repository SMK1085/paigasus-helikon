//! `FrameBudget` — keeps outbound WebSocket traffic inside AgentCore's frame-size and
//! frame-rate quotas. Shared by the HTTP-protocol and AG-UI `/ws` endpoints.

use std::collections::VecDeque;
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
/// AgentCore closes the connection above **250 frames/second**. `FrameBudget` enforces
/// this as a true trailing window — no rolling one-second span, evaluated at any
/// instant, ever contains more than this many admitted frames — rather than a fixed
/// window that resets to zero on a clock tick, which would let two capfuls land back
/// to back across a reset and double the effective rate right at the boundary.
pub(crate) const FRAME_RATE_CAP: u32 = 200;

/// Slack left around a split field's own quotes and separators, on top of the measured
/// envelope, so a chunk cannot overrun the cap through JSON punctuation alone.
const SPLIT_HEADROOM_BYTES: usize = 16;

/// How an oversize frame is broken up.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SplitStrategy {
    /// Split one string field's value across several otherwise-identical frames. The
    /// result is N valid protocol events, so a client needs no reassembly logic.
    // The HTTP-protocol `/ws` endpoint (SMA-461 Task 3) uses only `Envelope` — no
    // event type it streams has a single dominant text field worth splitting in
    // place. The AG-UI `/ws` endpoint (SMA-461 Task 7, src/agui/ws.rs) is the real
    // caller of this variant, for its `TEXT_MESSAGE_CONTENT` delta events.
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
    /// Emission timestamps inside the trailing one-second window, oldest first.
    /// Bounded at `FRAME_RATE_CAP` entries by construction.
    recent: VecDeque<tokio::time::Instant>,
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
            recent: VecDeque::with_capacity(FRAME_RATE_CAP as usize),
            chunk_group: 0,
        }
    }

    /// Turn one logical event into the wire-ready text frames for it.
    ///
    /// Always returns at least one frame. Every returned frame is at most
    /// `MAX_FRAME_BYTES` serialized bytes.
    ///
    /// **Deliberately synchronous, and deliberately not paced.** Call
    /// [`tick`](FrameBudget::tick) immediately *before writing each* returned frame, so
    /// the delay lands between writes. An earlier version did all the waiting inside
    /// this method and handed back the whole batch, which meant the caller then wrote
    /// every frame back to back: the connection slept for the accumulated delay and
    /// then put the entire burst on the wire in one go — precisely the shape
    /// `FRAME_RATE_CAP` exists to prevent, since the pacer's bookkeeping stayed correct
    /// while the wire timing did not. Splitting the two makes that mistake unavailable:
    /// this function cannot await, so pacing can only happen at the send site.
    pub(crate) fn frames(&mut self, frame: Value) -> Vec<String> {
        self.split(frame)
    }

    /// Consume one frame from the rate budget, sleeping until the trailing
    /// one-second window has room.
    ///
    /// Call immediately before writing the frame this reserves capacity for — see
    /// [`frames`](FrameBudget::frames).
    ///
    /// A trailing window (rather than a fixed resetting window) is required: a
    /// fixed window admits up to 2x the cap across a reset boundary, which a real
    /// sliding-window limiter on AgentCore's side would reject.
    pub(crate) async fn tick(&mut self) {
        const WINDOW: Duration = Duration::from_secs(1);
        loop {
            let now = tokio::time::Instant::now();
            while let Some(&front) = self.recent.front() {
                if now.duration_since(front) >= WINDOW {
                    self.recent.pop_front();
                } else {
                    break;
                }
            }
            if self.recent.len() < FRAME_RATE_CAP as usize {
                self.recent.push_back(now);
                return;
            }
            let front = *self
                .recent
                .front()
                .expect("non-empty: len >= FRAME_RATE_CAP >= 1");
            tokio::time::sleep(WINDOW - now.duration_since(front)).await;
        }
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
    /// Falls back to the envelope strategy when the field is absent, not a string, or
    /// empty — in each case there is no field content to split, but the frame is
    /// oversize (for some other reason, e.g. another field) and must still reach the
    /// wire rather than being silently dropped.
    fn split_content(&mut self, frame: Value, field: &str) -> Vec<String> {
        let Some(text) = frame
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return self.split_envelope(&frame.to_string());
        };
        // Budget for the envelope around the field: serialize the event with the field
        // emptied and subtract, leaving room for escaping growth inside the chunk.
        let mut probe = frame.clone();
        probe[field] = Value::String(String::new());
        let overhead = probe.to_string().len();
        // The event's *other* fields already fill the frame: there is no budget left to
        // put content in. Splitting in place would emit one still-oversize frame per
        // character, breaking the documented size guarantee and exploding the frame count.
        // The envelope strategy is the only one that can bound this.
        if overhead + SPLIT_HEADROOM_BYTES >= MAX_FRAME_BYTES {
            return self.split_envelope(&frame.to_string());
        }
        // Worst case each char serializes to 6 bytes ("\uXXXX"), so budget conservatively.
        let budget = MAX_FRAME_BYTES.saturating_sub(overhead + SPLIT_HEADROOM_BYTES) / 6;
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

    /// Regression (CodeRabbit, PR #186): when the event's *other* fields alone fill the
    /// frame, in-place splitting has no budget left — it emitted one still-oversize
    /// frame per character. The envelope strategy has to take over.
    #[tokio::test]
    async fn a_split_field_with_no_room_left_falls_back_to_the_envelope() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        // `messageId` alone exceeds the cap, so no chunk of `delta` can ever fit
        // alongside it.
        let out = b.frames(json!({
            "type": "TEXT_MESSAGE_CONTENT",
            "messageId": big_text(MAX_FRAME_BYTES + 5_000),
            "delta": big_text(50_000),
        }));

        assert!(out.len() > 1, "an oversize frame must be split");
        for f in &out {
            assert!(
                f.len() <= MAX_FRAME_BYTES,
                "emitted frame of {} bytes exceeds the cap",
                f.len()
            );
        }
        assert!(
            out.len() < 10_000,
            "one frame per character means the budget collapsed: {} frames",
            out.len()
        );
        let reassembled: String = out
            .iter()
            .map(|f| {
                let v: Value = serde_json::from_str(f).unwrap();
                assert_eq!(v["type"], "helikon.chunk");
                v["data"].as_str().unwrap().to_owned()
            })
            .collect();
        let inner: Value = serde_json::from_str(&reassembled).unwrap();
        assert_eq!(inner["type"], "TEXT_MESSAGE_CONTENT");
    }

    /// Regression (CodeRabbit, PR #186): splitting must not consume the rate budget.
    /// Pacing belongs at the send site, one `tick` per write — if `frames` charged the
    /// budget itself, the caller would sleep for the whole batch and then burst every
    /// frame onto the wire at once.
    #[tokio::test(start_paused = true)]
    async fn splitting_does_not_consume_the_rate_budget() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let start = tokio::time::Instant::now();

        // Split several oversize events without writing any of them.
        for _ in 0..5 {
            let out = b.frames(
                json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": big_text(500_000)}),
            );
            assert!(out.len() > 1);
        }
        assert_eq!(
            start.elapsed(),
            Duration::ZERO,
            "splitting must not await anything"
        );

        // A full window's worth of writes is still available afterwards.
        for _ in 0..FRAME_RATE_CAP {
            b.tick().await;
        }
        assert_eq!(
            start.elapsed(),
            Duration::ZERO,
            "splitting must not have charged the rate budget"
        );
    }

    #[tokio::test]
    async fn small_frame_passes_through_unwrapped() {
        let mut b = FrameBudget::new();
        let out = b.frames(json!({"type": "RUN_STARTED", "runId": "r1"}));
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
        let out = b.frames(
            json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": big_text(500_000)}),
        );
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
        let out =
            b.frames(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": original}));
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
        let out =
            b.frames(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": original}));
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
        let out =
            b.frames(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": payload}));
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
        let out = b.frames(json!({"type": "TOOL_CALL_RESULT", "content": big_text(200_000)}));
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

    /// An oversize frame whose split field is empty must still reach the wire:
    /// returning zero frames would drop the event silently.
    #[tokio::test]
    async fn an_oversize_frame_with_an_empty_split_field_still_emits() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let out = b.frames(json!({
            "type": "TEXT_MESSAGE_CONTENT",
            "messageId": big_text(200_000),
            "delta": ""
        }));
        assert!(!out.is_empty(), "an oversize frame must never be dropped");
        for f in &out {
            assert!(
                f.len() <= MAX_FRAME_BYTES,
                "frame of {} bytes too large",
                f.len()
            );
        }
    }

    /// Deterministic pacing: with the clock paused, admitting more frames than the
    /// per-second cap must have awaited a total delay of at least one second. Asserting
    /// on the virtual clock (not wall time) keeps this stable across the CI matrix.
    #[tokio::test(start_paused = true)]
    async fn pacer_delays_once_the_rate_cap_is_reached() {
        let mut b = FrameBudget::new();
        let start = tokio::time::Instant::now();
        for i in 0..(FRAME_RATE_CAP + 1) {
            b.tick().await;
            let _ = b.frames(json!({"type": "STEP_STARTED", "n": i}));
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
            b.tick().await;
            let _ = b.frames(json!({"type": "TOOL_CALL_RESULT", "toolCallId": format!("t{i}")}));
        }
        assert!(start.elapsed() >= std::time::Duration::from_secs(1));
    }

    /// No trailing one-second window may ever contain more than the cap. A fixed
    /// resetting window passes the two burst tests above but fails this one.
    #[tokio::test(start_paused = true)]
    async fn no_trailing_one_second_window_exceeds_the_rate_cap() {
        let mut b = FrameBudget::new();
        let mut stamps = Vec::new();
        for i in 0..(FRAME_RATE_CAP * 2 + 5) {
            b.tick().await;
            let _ = b.frames(json!({"type": "STEP_STARTED", "n": i}));
            stamps.push(tokio::time::Instant::now());
        }
        for (i, t) in stamps.iter().enumerate() {
            let in_window = stamps[..=i]
                .iter()
                .filter(|s| t.duration_since(**s) < std::time::Duration::from_secs(1))
                .count();
            assert!(
                in_window <= FRAME_RATE_CAP as usize,
                "frame {i}: {in_window} frames inside the trailing second, cap is {FRAME_RATE_CAP}"
            );
        }
    }

    /// A deliberately-constructed boundary straddle: land a capful 900ms into the
    /// first window (a manual clock advance, not incidental scheduling, puts it
    /// exactly there), then burst a second capful the instant the window resets.
    ///
    /// This is the scenario `no_trailing_one_second_window_exceeds_the_rate_cap`
    /// above was meant to catch, but under a fully deterministic paused clock with
    /// no artificial jitter, consecutive fixed-window resets land exactly 1.000s
    /// apart and that test's strict `<` comparison never sees them overlap — so it
    /// passes even against the old fixed-window `tick`, and doesn't by itself prove
    /// anything. Forcing the first batch away from the t=0 origin is what actually
    /// exposes the bug: the fixed window resets at t=1.0s regardless, so the second
    /// batch's t=1.0s frames sit only 100ms after the first batch's t=0.9s frames —
    /// both inside one real trailing second — and together exceed the cap.
    #[tokio::test(start_paused = true)]
    async fn a_boundary_straddling_burst_still_respects_the_trailing_window() {
        let mut b = FrameBudget::new();
        let mut stamps = Vec::new();

        tokio::time::advance(Duration::from_millis(900)).await;
        for i in 0..FRAME_RATE_CAP {
            b.tick().await;
            let _ = b.frames(json!({"type": "STEP_STARTED", "n": i}));
            stamps.push(tokio::time::Instant::now());
        }
        for i in 0..FRAME_RATE_CAP {
            b.tick().await;
            let _ = b.frames(json!({"type": "STEP_STARTED", "n": FRAME_RATE_CAP + i}));
            stamps.push(tokio::time::Instant::now());
        }

        for (i, t) in stamps.iter().enumerate() {
            let in_window = stamps[..=i]
                .iter()
                .filter(|s| t.duration_since(**s) < Duration::from_secs(1))
                .count();
            assert!(
                in_window <= FRAME_RATE_CAP as usize,
                "frame {i}: {in_window} frames inside the trailing second, cap is {FRAME_RATE_CAP}"
            );
        }
    }
}

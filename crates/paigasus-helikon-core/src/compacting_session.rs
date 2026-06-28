//! [`CompactingSession`] — a [`Session`] wrapper that LLM-summarizes the log
//! once a token threshold is exceeded. See spec §4.2.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;

use crate::{
    project, CancellationToken, ContentPart, ConversationSnapshot, HeuristicTokenCounter, Item,
    Model, ModelEvent, ModelRequest, ModelSettings, SequenceId, Session, SessionError,
    SessionEvent, TokenCounter,
};

const DEFAULT_PROMPT: &str = "Summarize the conversation so far into a concise summary, \
preserving key facts, decisions, and open questions.";
/// Default threshold (tokens). Chosen with headroom under common context windows.
const DEFAULT_THRESHOLD: usize = 8_000;

/// A [`Session`] that wraps any inner session and triggers LLM-based
/// compaction once the projected token count exceeds `threshold`.
///
/// **Single logical writer per session** (spec §4.2): the inner backend stays
/// durable under concurrency, but the compaction bookkeeping assumes appends
/// through this wrapper are serialized. `threshold` must sit below the
/// summarization model's context window, and the model should produce
/// summaries shorter than `threshold`, for compaction to converge.
pub struct CompactingSession<S> {
    inner: S,
    model: Arc<dyn Model>,
    counter: Arc<dyn TokenCounter>,
    threshold: usize,
    settings: ModelSettings,
    prompt: String,
    cheap_estimate: AtomicUsize,
    compacting: AtomicBool,
}

// Manual Debug: `Arc<dyn Model>` is not Debug (Model has no Debug bound).
impl<S: std::fmt::Debug> std::fmt::Debug for CompactingSession<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactingSession")
            .field("inner", &self.inner)
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

/// Builder for [`CompactingSession`].
pub struct CompactingSessionBuilder<S> {
    inner: S,
    model: Arc<dyn Model>,
    counter: Option<Arc<dyn TokenCounter>>,
    threshold: usize,
    settings: ModelSettings,
    prompt: String,
}

impl<S: std::fmt::Debug> std::fmt::Debug for CompactingSessionBuilder<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactingSessionBuilder")
            .field("inner", &self.inner)
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

/// Error constructing a [`CompactingSession`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompactingSessionError {
    /// `threshold` was 0 (would never compact).
    #[error("CompactingSession threshold must be greater than zero")]
    ZeroThreshold,
}

impl<S: Session> CompactingSession<S> {
    /// Start building a [`CompactingSession`] wrapping `inner`, summarizing via `model`.
    pub fn builder(inner: S, model: Arc<dyn Model>) -> CompactingSessionBuilder<S> {
        CompactingSessionBuilder {
            inner,
            model,
            counter: None,
            threshold: DEFAULT_THRESHOLD,
            settings: ModelSettings::default(),
            prompt: DEFAULT_PROMPT.to_owned(),
        }
    }
}

impl<S: Session> CompactingSessionBuilder<S> {
    /// Token threshold above which compaction fires. Must be > 0.
    pub fn threshold(mut self, t: usize) -> Self {
        self.threshold = t;
        self
    }
    /// Override the token counter (default [`HeuristicTokenCounter`]).
    pub fn counter(mut self, c: Arc<dyn TokenCounter>) -> Self {
        self.counter = Some(c);
        self
    }
    /// Override the summarization instruction prompt.
    pub fn prompt(mut self, p: impl Into<String>) -> Self {
        self.prompt = p.into();
        self
    }
    /// Override the model settings used for the summarization call.
    pub fn settings(mut self, s: ModelSettings) -> Self {
        self.settings = s;
        self
    }
    /// Build the [`CompactingSession`], or fail on an invalid configuration.
    pub fn build(self) -> Result<CompactingSession<S>, CompactingSessionError> {
        if self.threshold == 0 {
            return Err(CompactingSessionError::ZeroThreshold);
        }
        Ok(CompactingSession {
            inner: self.inner,
            model: self.model,
            counter: self
                .counter
                .unwrap_or_else(|| Arc::new(HeuristicTokenCounter)),
            threshold: self.threshold,
            settings: self.settings,
            prompt: self.prompt,
            // usize::MAX forces the first maybe_compact to take the authoritative
            // path and seed from the (possibly pre-populated) inner log.
            cheap_estimate: AtomicUsize::new(usize::MAX),
            compacting: AtomicBool::new(false),
        })
    }
}

/// RAII reset for the single-flight `compacting` flag — constructed only on the
/// swap-won path, so it never clears a flag it did not set.
struct CompactGuard<'a>(&'a AtomicBool);
impl Drop for CompactGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl<S: Session> CompactingSession<S> {
    fn add_estimate(&self, events: &[SessionEvent]) {
        // Cheap char estimate of new events (over-approx: counts handoffs as 0 text).
        let snap = project(events);
        let chars: usize = self.counter.count(&snap.messages) * 4; // counter returns ~chars/4
        let prev = self.cheap_estimate.load(Ordering::Relaxed);
        self.cheap_estimate
            .store(prev.saturating_add(chars), Ordering::Relaxed);
    }

    async fn maybe_compact(&self) -> Result<(), SessionError> {
        // 1. Cheap gate (usize::MAX on first call forces an authoritative read).
        if self.cheap_estimate.load(Ordering::Relaxed) <= self.threshold.saturating_mul(4) {
            return Ok(());
        }
        // 2. Single-flight.
        if self
            .compacting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let _guard = CompactGuard(&self.compacting);

        // 3. Authoritative count.
        let evs = self.inner.events(None).await?;
        let snap = project(&evs);
        let tokens = self.counter.count(&snap.messages);
        if tokens <= self.threshold {
            self.cheap_estimate
                .store(tokens.saturating_mul(4), Ordering::Relaxed);
            return Ok(());
        }
        // 5. Nothing useful to collapse: empty, or the single message is already
        // a running System summary (re-compacting it would loop forever).
        // A lone UserMessage/AssistantMessage SHOULD be compacted.
        if snap.messages.is_empty()
            || (snap.messages.len() == 1 && matches!(snap.messages[0], Item::System { .. }))
        {
            return Ok(());
        }
        // 6. live = events since (and incl.) the last Compacted marker.
        let live = live_count(&evs);

        // 7. Summarize.
        let mut messages = snap.messages.clone();
        messages.push(Item::UserMessage {
            content: vec![ContentPart::Text {
                text: self.prompt.clone(),
            }],
        });
        let req = ModelRequest {
            messages,
            tools: Vec::new(),
            model_settings: self.settings.clone(),
        };
        let summary = match self.collect_summary(req).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "CompactingSession: summarization failed; skipping compaction");
                return Ok(());
            }
        };
        // 8. Empty-summary guard.
        if summary.trim().is_empty() {
            tracing::warn!("CompactingSession: model returned empty summary; skipping compaction");
            return Ok(());
        }
        // 9. Append marker; resync cheap estimate to the summary size.
        self.inner
            .append(&[SessionEvent::compacted(summary.clone(), live as u64)])
            .await?;
        let summary_item = Item::System {
            content: vec![ContentPart::Text { text: summary }],
        };
        self.cheap_estimate.store(
            self.counter.count(&[summary_item]).saturating_mul(4),
            Ordering::Relaxed,
        );
        Ok(())
    }

    async fn collect_summary(&self, req: ModelRequest) -> Result<String, SessionError> {
        let mut stream = self
            .model
            .invoke(req, CancellationToken::new())
            .await
            .map_err(|e| SessionError::Other(e.into()))?;
        let mut summary = String::new();
        while let Some(ev) = stream.next().await {
            match ev.map_err(|e| SessionError::Other(e.into()))? {
                ModelEvent::TokenDelta { text } => summary.push_str(&text),
                ModelEvent::Finish { .. } => break,
                _ => {}
            }
        }
        Ok(summary)
    }
}

/// Count of events since (and including) the last `Compacted`; full length if none.
fn live_count(evs: &[SessionEvent]) -> usize {
    let last_compacted = evs
        .iter()
        .rposition(|e| matches!(e, SessionEvent::Compacted { .. }));
    match last_compacted {
        Some(i) => evs.len() - i,
        None => evs.len(),
    }
}

#[async_trait]
impl<S: Session> Session for CompactingSession<S> {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        self.inner.append(events).await?;
        self.add_estimate(events);
        self.maybe_compact().await?;
        Ok(())
    }
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        self.inner.events(since).await
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        self.inner.snapshot().await
    }
}

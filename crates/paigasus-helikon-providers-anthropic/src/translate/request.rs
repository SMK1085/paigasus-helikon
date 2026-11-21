//! `Vec<Item>` + `ModelRequest` → Anthropic Messages request body.
//!
//! Rules per the SMA-317 spec § "Wire translation". The translator
//! produces a `serde_json::Value` rather than typed structs to keep the
//! wire-snapshot tests readable.
//!
//! SMA-840: an assistant message can hold `ProviderData` parts of this crate.
//! They carry the `thinking` and `redacted_thinking` blocks of the response,
//! and the positions of its first `text` block and its `tool_use` blocks. The
//! translator sends the blocks back and puts the blocks of each source
//! message in their response order. A history with no such part builds as
//! before.

use std::collections::{HashMap, VecDeque};

use paigasus_helikon_core::{ContentPart, Item};
use serde_json::{json, Value};

use crate::provider_data::{self, AnthropicData};

/// Output of [`translate_messages`]: the top-level `system` field (string
/// or block-array form) and the `messages` array.
pub(crate) struct TranslatedMessages {
    pub(crate) system: Option<Value>,
    pub(crate) messages: Value,
    /// `true` when the last user message produced no message, no kept
    /// message or tool item follows it (dropped assistant items do not
    /// count), and the last sent message is an assistant one.
    pub(crate) trailing_message_dropped: bool,
}

/// The sort key of a block inside its group.
#[derive(Debug, Clone, Copy)]
enum SortKey {
    /// `(index, 1)`, where `index` is the content-block index of the
    /// response. A `tool_use` block with no position has `(u64::MAX, 1)`.
    At(u64, u8),
    /// A text block of a group with no `text` position. The key becomes
    /// `(k, 0)`, where `k` is the smallest keyed `tool_use` index of the group
    /// in the same wire message, or `u64::MAX` when there is none.
    BeforeFirstToolUse,
}

impl SortKey {
    fn resolve(self, first_tool_use: u64) -> (u64, u8) {
        match self {
            Self::At(index, rank) => (index, rank),
            Self::BeforeFirstToolUse => (first_tool_use, 0),
        }
    }
}

/// The place of a block in the response order of its source message.
#[derive(Debug, Clone, Copy)]
struct Slot {
    /// The source message of the block. Keys of different groups are never
    /// compared.
    group: usize,
    key: SortKey,
}

/// One content block of a wire message. Only `value` reaches the JSON.
#[derive(Debug, Clone, Default)]
struct WireBlock {
    value: Value,
    /// `None` for a block that does not move in the final sort.
    slot: Option<Slot>,
}

impl WireBlock {
    fn plain(value: Value) -> Self {
        Self { value, slot: None }
    }

    fn kind(&self) -> &str {
        self.value["type"].as_str().unwrap_or("")
    }

    fn is_tool_use(&self) -> bool {
        self.kind() == "tool_use"
    }

    fn is_thinking(&self) -> bool {
        matches!(self.kind(), "thinking" | "redacted_thinking")
    }
}

/// One message of the `messages` array, before it becomes JSON.
#[derive(Debug, Clone)]
struct WireMessage {
    role: &'static str,
    content: Vec<WireBlock>,
}

impl WireMessage {
    /// `true` when the message holds a `text` or a `tool_use` block.
    fn has_text_or_tool_use(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b.kind(), "text" | "tool_use"))
    }

    fn into_value(mut self) -> Value {
        sort_groups(&mut self.content);
        let content: Vec<Value> = self.content.into_iter().map(|b| b.value).collect();
        json!({"role": self.role, "content": content})
    }
}

/// The Anthropic positions of one source message: an `AssistantMessage`
/// with at least one valid part of this crate, together with the
/// `Item::ToolCall` items that follow it before the next `AssistantMessage`
/// or the next user message with a part that is not a tool result.
#[derive(Debug, Clone)]
struct Source {
    group: usize,
    /// The wire message that holds the blocks of the `AssistantMessage`.
    /// `None` when it gave no block.
    wire: Option<usize>,
    /// The `tool_use` positions by call id, in the order of the parts.
    tool_positions: HashMap<String, VecDeque<u64>>,
}

impl Source {
    /// The key of the next `tool_use` block with `call_id`. Each position is
    /// used once, so a repeated id takes its positions in order.
    fn tool_key(&mut self, call_id: &str) -> SortKey {
        let index = self
            .tool_positions
            .get_mut(call_id)
            .and_then(VecDeque::pop_front);
        SortKey::At(index.unwrap_or(u64::MAX), 1)
    }
}

/// The translator state before the item that made a tentative message.
#[derive(Debug)]
struct Saved {
    item: usize,
    messages_len: usize,
    system_len: usize,
    pending_tool_use: Vec<WireBlock>,
    pending_tool_results: Vec<Value>,
    last_item_dropped: bool,
    next_group: usize,
    source: Option<Source>,
}

/// A wire assistant message that holds only thinking blocks so far.
///
/// It stays the last message until a message is pushed after it. If it then
/// (or at the end) still has no `text` and no `tool_use` block, the
/// translator goes back to `saved`, drops the thinking blocks of the item,
/// and translates again from that item. The item then counts as empty, as it
/// did before SMA-840: no flush, no message, and no change to the flag.
#[derive(Debug)]
struct Tentative {
    msg: usize,
    saved: Saved,
}

/// The signal to go back to the saved state of the tentative message.
struct Rollback;

#[derive(Debug, Default)]
struct Translator {
    system_text: String,
    messages: Vec<WireMessage>,
    pending_tool_use: Vec<WireBlock>,
    pending_tool_results: Vec<Value>,
    last_item_dropped: bool,
    next_group: usize,
    source: Option<Source>,
    tentative: Option<Tentative>,
}

/// Translate the conversation into Anthropic's request shape.
///
/// `system` is `None` when no `Item::System` has non-blank text. `messages`
/// is always an array. Text parts that are empty or whitespace only, and
/// messages left with no blocks, are dropped because the API rejects them.
///
/// A wire assistant message that has only thinking blocks after all its
/// `tool_use` blocks join drops them, and then counts as empty. Decisions go
/// in item order: each drop translates the items after it again.
pub(crate) fn translate_messages(items: &[Item]) -> TranslatedMessages {
    let mut t = Translator::default();
    let mut drop_thinking = vec![false; items.len()];
    let mut i = 0;
    loop {
        let done = i == items.len();
        let step = if done {
            t.finish()
        } else {
            t.step(i, &items[i], drop_thinking[i])
        };
        match step {
            Ok(()) if done => break,
            Ok(()) => i += 1,
            Err(Rollback) => {
                let tentative = t
                    .tentative
                    .take()
                    .expect("a rollback needs a tentative message");
                i = tentative.saved.item;
                // Each rollback drops one more item, so the loop ends.
                drop_thinking[i] = true;
                t.restore(tentative.saved);
            }
        }
    }
    t.into_output()
}

impl Translator {
    fn step(&mut self, i: usize, item: &Item, drop_thinking: bool) -> Result<(), Rollback> {
        match item {
            Item::System { content } => {
                self.flush_pending_assistant()?;
                self.flush_pending_user()?;
                if !self.system_text.is_empty() {
                    self.system_text.push('\n');
                }
                self.system_text.push_str(&text_of(content));
            }
            Item::UserMessage { content } => {
                // A user message with a part that is not a tool result ends
                // the source message (the SMA-827 Gemini rule).
                if content
                    .iter()
                    .any(|p| !matches!(p, ContentPart::ToolResult { .. }))
                {
                    self.source = None;
                }
                self.flush_pending_assistant()?;
                // Pending tool_results go at the front of this user turn.
                let mut blocks: Vec<Value> = std::mem::take(&mut self.pending_tool_results);
                blocks.extend(user_blocks(content));
                self.last_item_dropped = blocks.is_empty();
                if !blocks.is_empty() {
                    let blocks = blocks.into_iter().map(WireBlock::plain).collect();
                    self.push_message("user", blocks)?;
                }
            }
            Item::AssistantMessage { content, agent: _ } => {
                let (mut blocks, source) = assistant_blocks(content, self.next_group);
                if drop_thinking {
                    blocks.retain(|b| !b.is_thinking());
                }
                // An assistant item with no blocks acts as if it were absent:
                // it flushes nothing (so an open tool round stays whole) and
                // leaves the flag as it is (a drop of the user turn before
                // it still counts). A kept assistant message ends the drop.
                //
                // A message of only tool uses and thinking blocks is the
                // nested form of `Item::ToolCall`, or the accumulator form of
                // a thinking round (SMA-834, SMA-840). Inside an open round it
                // joins that round, as a call does. A flush would put a user
                // turn that lacks some results right after the open assistant
                // turn. On this path the last message is an assistant message
                // (`round_is_answered` is `true` for an empty trailing
                // assistant run). `pending_tool_use` is empty, because each
                // `ToolResult` flushes it. So the append keeps the order.
                // Setting `last_item_dropped` here is for symmetry. The flag
                // is already `false`, because a `ToolResult` came after any
                // dropped user message.
                let only_tool_uses =
                    !blocks.is_empty() && blocks.iter().all(|b| b.is_tool_use() || b.is_thinking());
                let joins = only_tool_uses
                    && !self.pending_tool_results.is_empty()
                    && !round_is_answered(&self.messages, &self.pending_tool_results);
                let thinking_only = !blocks.is_empty() && blocks.iter().all(WireBlock::is_thinking);
                let saved = (thinking_only && !joins).then(|| self.save(i));
                if source.is_some() {
                    self.next_group += 1;
                }
                self.source = source;
                if joins {
                    self.last_item_dropped = false;
                    let target = self.tool_use_target();
                    if let Some(source) = &mut self.source {
                        source.wire = Some(target);
                    }
                    self.push_tool_uses(blocks);
                } else if !blocks.is_empty() {
                    self.flush_pending_assistant()?;
                    self.flush_pending_user()?;
                    self.last_item_dropped = false;
                    let msg = self.messages.len();
                    self.push_message("assistant", blocks)?;
                    if let Some(source) = &mut self.source {
                        source.wire = Some(msg);
                    }
                    if let Some(saved) = saved {
                        self.tentative = Some(Tentative { msg, saved });
                    }
                }
            }
            Item::ToolCall {
                call_id,
                name,
                args,
            } => {
                self.last_item_dropped = false;
                // A new tool_use follows any results already seen. Close
                // that user turn first only when the last assistant round is
                // fully answered; otherwise the call joins that round.
                if round_is_answered(&self.messages, &self.pending_tool_results) {
                    self.flush_pending_user()?;
                }
                // The call keeps its key only in the wire message that holds
                // the blocks of its source message.
                let target = self.tool_use_target();
                let slot = self.source.as_mut().and_then(|source| {
                    let key = source.tool_key(call_id);
                    (source.wire == Some(target)).then_some(Slot {
                        group: source.group,
                        key,
                    })
                });
                let value = json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": args,
                });
                self.push_tool_uses(vec![WireBlock { value, slot }]);
            }
            Item::ToolResult { call_id, content } => {
                self.last_item_dropped = false;
                self.flush_pending_assistant()?;
                self.pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": text_of(content),
                }));
            }
            _ => {
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "unknown Item variant; skipping",
                );
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Rollback> {
        self.flush_pending_assistant()?;
        self.flush_pending_user()?;
        match &self.tentative {
            Some(t) if !self.messages[t.msg].has_text_or_tool_use() => Err(Rollback),
            _ => Ok(()),
        }
    }

    /// Push a message. A tentative message before it is then complete: keep
    /// it when it has a `text` or `tool_use` block, else go back.
    fn push_message(
        &mut self,
        role: &'static str,
        content: Vec<WireBlock>,
    ) -> Result<(), Rollback> {
        if let Some(t) = &self.tentative {
            debug_assert_eq!(
                t.msg + 1,
                self.messages.len(),
                "a tentative message is last"
            );
            if !self.messages[t.msg].has_text_or_tool_use() {
                return Err(Rollback);
            }
            self.tentative = None;
        }
        self.messages.push(WireMessage { role, content });
        Ok(())
    }

    fn flush_pending_assistant(&mut self) -> Result<(), Rollback> {
        if self.pending_tool_use.is_empty() {
            return Ok(());
        }
        let content = std::mem::take(&mut self.pending_tool_use);
        self.push_message("assistant", content)
    }

    fn flush_pending_user(&mut self) -> Result<(), Rollback> {
        if self.pending_tool_results.is_empty() {
            return Ok(());
        }
        let content = std::mem::take(&mut self.pending_tool_results)
            .into_iter()
            .map(WireBlock::plain)
            .collect();
        self.push_message("user", content)
    }

    /// The index of the wire message that `push_tool_uses` adds to now.
    fn tool_use_target(&self) -> usize {
        match self.messages.last() {
            Some(m) if m.role == "assistant" => self.messages.len() - 1,
            // Queued blocks flush into the next message.
            _ => self.messages.len(),
        }
    }

    /// Add `tool_use` blocks to the open assistant turn. When the last
    /// message is an assistant message, extend it. Otherwise queue them.
    fn push_tool_uses(&mut self, blocks: Vec<WireBlock>) {
        match self.messages.last_mut() {
            Some(last) if last.role == "assistant" => last.content.extend(blocks),
            _ => self.pending_tool_use.extend(blocks),
        }
    }

    fn save(&self, item: usize) -> Saved {
        Saved {
            item,
            messages_len: self.messages.len(),
            system_len: self.system_text.len(),
            pending_tool_use: self.pending_tool_use.clone(),
            pending_tool_results: self.pending_tool_results.clone(),
            last_item_dropped: self.last_item_dropped,
            next_group: self.next_group,
            source: self.source.clone(),
        }
    }

    /// Go back to `saved`. The messages before `saved.messages_len` did not
    /// change after the save: only the last message gets new blocks, and the
    /// tentative message and all later ones are at or after that length.
    fn restore(&mut self, saved: Saved) {
        self.messages.truncate(saved.messages_len);
        self.system_text.truncate(saved.system_len);
        self.pending_tool_use = saved.pending_tool_use;
        self.pending_tool_results = saved.pending_tool_results;
        self.last_item_dropped = saved.last_item_dropped;
        self.next_group = saved.next_group;
        self.source = saved.source;
        self.tentative = None;
    }

    fn into_output(self) -> TranslatedMessages {
        let trailing_message_dropped =
            self.last_item_dropped && self.messages.last().is_some_and(|m| m.role == "assistant");
        let system = if self.system_text.trim().is_empty() {
            None
        } else {
            Some(Value::String(self.system_text))
        };
        let messages = self
            .messages
            .into_iter()
            .map(WireMessage::into_value)
            .collect();
        TranslatedMessages {
            system,
            messages: Value::Array(messages),
            trailing_message_dropped,
        }
    }
}

/// Put the blocks of each group back in response order.
///
/// For each group, take the positions its blocks have in the message, sort
/// those blocks by key (stable), and write them back into the same
/// positions. Blocks of other groups and blocks with no slot do not move.
fn sort_groups(content: &mut [WireBlock]) {
    let mut groups: Vec<usize> = Vec::new();
    for slot in content.iter().filter_map(|b| b.slot) {
        if !groups.contains(&slot.group) {
            groups.push(slot.group);
        }
    }
    for group in groups {
        let at: Vec<usize> = content
            .iter()
            .enumerate()
            .filter(|(_, b)| b.slot.is_some_and(|s| s.group == group))
            .map(|(i, _)| i)
            .collect();
        let first_tool_use = at
            .iter()
            .filter(|&&i| content[i].is_tool_use())
            .filter_map(|&i| match content[i].slot.map(|s| s.key) {
                Some(SortKey::At(index, _)) if index != u64::MAX => Some(index),
                _ => None,
            })
            .min()
            .unwrap_or(u64::MAX);
        let mut taken: Vec<WireBlock> = at
            .iter()
            .map(|&i| std::mem::take(&mut content[i]))
            .collect();
        taken.sort_by_key(|b| b.slot.map(|s| s.key.resolve(first_tool_use)));
        for (i, block) in at.into_iter().zip(taken) {
            content[i] = block;
        }
    }
}

/// `true` when every `tool_use` id in the trailing run of assistant messages
/// has a matching `tool_result` in `results`. The API joins adjacent
/// assistant messages into one turn, so the whole run is one round. Also
/// `true` when there is nothing to compare (no assistant message last), so
/// the results then flush as before.
fn round_is_answered(messages: &[WireMessage], results: &[Value]) -> bool {
    messages
        .iter()
        .rev()
        .take_while(|m| m.role == "assistant")
        .flat_map(|m| &m.content)
        .filter(|b| b.is_tool_use())
        .all(|b| results.iter().any(|r| r["tool_use_id"] == b.value["id"]))
}

/// The API rejects empty and whitespace-only text blocks.
fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

fn text_of(parts: &[ContentPart]) -> String {
    let mut s = String::new();
    for p in parts {
        if let ContentPart::Text { text } = p {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(text);
        }
    }
    s
}

fn user_blocks(content: &[ContentPart]) -> Vec<Value> {
    use paigasus_helikon_core::MediaSource;
    content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } if is_blank(text) => None,
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::ToolResult { call_id, content } => Some(json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": text_of(content),
            })),
            ContentPart::Image { source } => match source {
                MediaSource::Url { url } => Some(json!({
                    "type": "image",
                    "source": {"type": "url", "url": url},
                })),
                MediaSource::Base64 { mime_type, data } => Some(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": mime_type,
                        "data": data,
                    },
                })),
                _ => {
                    tracing::warn!(
                        target: "paigasus::anthropic::translate",
                        "unsupported MediaSource variant; skipping image",
                    );
                    None
                }
            },
            // Provider data from another provider has no Anthropic form.
            ContentPart::ProviderData { .. } => None,
            _ => None,
        })
        .collect()
}

/// The blocks of one `AssistantMessage`, and its source state.
///
/// The source state is `Some` only when at least one part of this crate is
/// valid. Then each block gets a slot in group `group`. Else no block gets a
/// slot, and the message builds as before SMA-840.
fn assistant_blocks(content: &[ContentPart], group: usize) -> (Vec<WireBlock>, Option<Source>) {
    // Read each part once, so that a skipped part logs once.
    let data: Vec<Option<(AnthropicData, Option<&str>)>> =
        content.iter().map(provider_data::from_part).collect();
    let mut source = data.iter().any(Option::is_some).then(|| Source {
        group,
        wire: None,
        tool_positions: HashMap::new(),
    });
    let mut text_key = SortKey::BeforeFirstToolUse;
    if let Some(source) = &mut source {
        let mut text_seen = false;
        for (d, call_id) in data.iter().flatten() {
            match d {
                // The stream writes a position for the first text block only.
                AnthropicData::Text { index } if !text_seen => {
                    text_seen = true;
                    text_key = SortKey::At(u64::from(*index), 1);
                }
                AnthropicData::ToolUse { index } => match call_id {
                    Some(id) => source
                        .tool_positions
                        .entry((*id).to_owned())
                        .or_default()
                        .push_back(u64::from(*index)),
                    None => tracing::debug!(
                        target: "paigasus::anthropic::translate",
                        index = *index,
                        "skipping anthropic tool_use position with no call id",
                    ),
                },
                _ => {}
            }
        }
    }
    let group = source.as_ref().map(|s| s.group);
    let slot = |key| group.map(|group| Slot { group, key });
    let mut blocks = Vec::new();
    for (part, data) in content.iter().zip(&data) {
        match part {
            ContentPart::Text { text } if is_blank(text) => {}
            // Core joins the text of a turn into one part. Each text block
            // of the group gets the same key, so the blocks stay in order.
            ContentPart::Text { text } => blocks.push(WireBlock {
                value: json!({"type": "text", "text": text}),
                slot: slot(text_key),
            }),
            ContentPart::ToolUse {
                call_id,
                name,
                args,
            } => {
                let slot = source.as_mut().map(|s| Slot {
                    group: s.group,
                    key: s.tool_key(call_id),
                });
                blocks.push(WireBlock {
                    value: json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": args,
                    }),
                    slot,
                });
            }
            // The thinking text travels in the `ProviderData` block part.
            ContentPart::Reasoning { .. } => {
                tracing::debug!(
                    target: "paigasus::anthropic::translate",
                    "dropping ContentPart::Reasoning on input; provider data carries the block",
                );
            }
            ContentPart::ProviderData { .. } => {
                // Provider data from another provider, and position parts,
                // give no block.
                if let Some((AnthropicData::Block { index, block }, _)) = data {
                    blocks.push(WireBlock {
                        value: block.to_wire(),
                        slot: slot(SortKey::At(u64::from(*index), 1)),
                    });
                }
            }
            _ => {}
        }
    }
    (blocks, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    #[test]
    fn system_collapses_into_top_level_string() {
        let items = vec![Item::System {
            content: vec![text("be helpful")],
        }];
        let out = translate_messages(&items);
        assert_eq!(out.system, Some(Value::String("be helpful".to_owned())));
        assert_eq!(out.messages, json!([]));
    }

    #[test]
    fn multiple_system_items_concatenate_in_order() {
        let items = vec![
            Item::System {
                content: vec![text("first")],
            },
            Item::UserMessage {
                content: vec![text("hi")],
            },
            Item::System {
                content: vec![text("second")],
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(
            out.system,
            Some(Value::String("first\nsecond".to_owned())),
            "all system items collapse into one top-level slot (order-loss vs surrounding turns)",
        );
    }

    #[test]
    fn user_text_emits_text_block() {
        let items = vec![Item::UserMessage {
            content: vec![text("hello")],
        }];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([{"role": "user", "content": [{"type": "text", "text": "hello"}]}]),
        );
    }

    #[test]
    fn assistant_text_emits_text_block() {
        let items = vec![Item::AssistantMessage {
            content: vec![text("done")],
            agent: Some("planner".to_owned()),
        }];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([{"role": "assistant", "content": [{"type": "text", "text": "done"}]}]),
        );
        // `agent` attribution is dropped (no Anthropic slot).
    }

    #[test]
    fn assistant_reasoning_is_always_dropped() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                },
                text("answer"),
            ],
            agent: None,
        }];
        let out = translate_messages(&items);
        let content = &out.messages[0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "answer");
    }

    #[test]
    fn tool_only_turn_then_assistant_text_keeps_order() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q")],
            },
            Item::ToolCall {
                call_id: "tu_1".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".into(),
                content: vec![text("pong")],
            },
            Item::AssistantMessage {
                content: vec![text("done")],
                agent: None,
            },
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
            ])
        );
    }

    #[test]
    fn tool_call_after_pending_result_does_not_fold_into_earlier_assistant() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q")],
            },
            Item::AssistantMessage {
                content: vec![text("a")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "tu_1".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".into(),
                content: vec![text("r1")],
            },
            Item::ToolCall {
                call_id: "tu_2".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_2".into(),
                content: vec![text("r2")],
            },
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "a"},
                    {"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}
                ]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "r1"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_2", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_2", "content": "r2"}]},
            ])
        );
    }

    #[test]
    fn multi_round_tool_loop_without_assistant_text_alternates() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q")],
            },
            Item::ToolCall {
                call_id: "tu_1".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".into(),
                content: vec![text("r1")],
            },
            Item::ToolCall {
                call_id: "tu_2".into(),
                name: "ping".into(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_2".into(),
                content: vec![text("r2")],
            },
            Item::AssistantMessage {
                content: vec![text("done")],
                agent: None,
            },
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "r1"}]},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_2", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_2", "content": "r2"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "done"}]},
            ])
        );
    }

    #[test]
    fn tool_call_folds_into_preceding_assistant() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![text("calling")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({"host": "ex.com"}),
            },
        ];
        let out = translate_messages(&items);
        let msg = &out.messages[0];
        assert_eq!(msg["role"], "assistant");
        let blocks = msg["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], json!({"type": "text", "text": "calling"}));
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "tu_1");
        assert_eq!(blocks[1]["name"], "ping");
        assert_eq!(blocks[1]["input"], json!({"host": "ex.com"}));
    }

    #[test]
    fn standalone_tool_calls_synthesize_assistant_carrier() {
        let items = vec![
            Item::ToolCall {
                call_id: "tu_a".to_owned(),
                name: "a".to_owned(),
                args: json!({}),
            },
            Item::ToolCall {
                call_id: "tu_b".to_owned(),
                name: "b".to_owned(),
                args: json!({"x": 1}),
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages.as_array().unwrap().len(), 1);
        let msg = &out.messages[0];
        assert_eq!(msg["role"], "assistant");
        let blocks = msg["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["id"], "tu_a");
        assert_eq!(blocks[1]["id"], "tu_b");
    }

    #[test]
    fn assistant_with_nested_tool_use_emits_tool_use_block() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                text("ok"),
                ContentPart::ToolUse {
                    call_id: "tu_x".to_owned(),
                    name: "search".to_owned(),
                    args: json!({"q": "rust"}),
                },
            ],
            agent: None,
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "tu_x");
        assert_eq!(blocks[1]["input"], json!({"q": "rust"}));
    }

    #[test]
    fn tool_result_coalesces_into_following_user_message() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "ping".to_owned(),
                    args: json!({}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
            Item::UserMessage {
                content: vec![text("now what?")],
            },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2, "must not produce consecutive user turns");
        assert_eq!(arr[0]["role"], "assistant");
        assert_eq!(arr[1]["role"], "user");
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0]["type"], "tool_result",
            "tool_result precedes text"
        );
        assert_eq!(blocks[0]["tool_use_id"], "tu_1");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "now what?");
    }

    /// SMA-804: a checkpoint `Inject` leaves a tool result directly followed
    /// by a user message. The translator folds both into one user turn.
    #[test]
    fn tool_result_then_user_message_request_shape() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("start")],
            },
            Item::AssistantMessage {
                content: vec![text("calling")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
            Item::UserMessage {
                content: vec![text("news")],
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "start"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"},
                    {"type": "text", "text": "news"},
                ]},
            ])
        );
    }

    fn user(t: &str) -> Item {
        Item::UserMessage {
            content: vec![text(t)],
        }
    }

    fn asst(t: &str) -> Item {
        Item::AssistantMessage {
            content: vec![text(t)],
            agent: None,
        }
    }

    fn call(id: &str) -> Item {
        Item::ToolCall {
            call_id: id.into(),
            name: "ping".into(),
            args: json!({}),
        }
    }

    fn result(id: &str) -> Item {
        Item::ToolResult {
            call_id: id.into(),
            content: vec![text("pong")],
        }
    }

    fn utext(t: &str) -> Value {
        json!({"role": "user", "content": [{"type": "text", "text": t}]})
    }

    fn atext(t: &str) -> Value {
        json!({"role": "assistant", "content": [{"type": "text", "text": t}]})
    }

    #[test]
    fn empty_text_part_is_dropped() {
        let out = translate_messages(&[user("")]);
        assert_eq!(out.messages, json!([]));
    }

    #[test]
    fn whitespace_only_text_part_is_dropped() {
        let out = translate_messages(&[user("  \n\t")]);
        assert_eq!(out.messages, json!([]));
    }

    #[test]
    fn blank_text_is_dropped_but_real_text_kept_verbatim() {
        let items = vec![Item::UserMessage {
            content: vec![text(""), text("  hi  "), text(" ")],
        }];
        assert_eq!(
            translate_messages(&items).messages,
            json!([utext("  hi  ")])
        );
    }

    #[test]
    fn blank_user_text_keeps_pending_tool_results() {
        let items = vec![user("q"), call("tu_1"), result("tu_1"), user("")];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"}]},
            ])
        );
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn blank_assistant_text_is_dropped() {
        let items = vec![Item::AssistantMessage {
            content: vec![text("  "), text("ok")],
            agent: None,
        }];
        assert_eq!(translate_messages(&items).messages, json!([atext("ok")]));
    }

    #[test]
    fn assistant_with_no_blocks_is_not_sent() {
        let items = vec![user("q"), asst(""), user("r")];
        assert_eq!(
            translate_messages(&items).messages,
            json!([utext("q"), utext("r")])
        );
    }

    #[test]
    fn reasoning_only_assistant_is_not_sent() {
        let items = vec![
            user("q"),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
            user("r"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([utext("q"), utext("r")])
        );
    }

    fn tool_turn_then_done() -> Value {
        json!([
            utext("q"),
            {"role": "assistant", "content": [{"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "pong"}]},
            atext("done"),
        ])
    }

    #[test]
    fn reasoning_only_assistant_before_tool_call_keeps_order() {
        let items = vec![
            user("q"),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
            call("tu_1"),
            result("tu_1"),
            asst("done"),
        ];
        assert_eq!(translate_messages(&items).messages, tool_turn_then_done());
    }

    #[test]
    fn whitespace_assistant_before_tool_call_keeps_order() {
        let items = vec![
            user("q"),
            asst("\n\n"),
            call("tu_1"),
            result("tu_1"),
            asst("done"),
        ];
        assert_eq!(translate_messages(&items).messages, tool_turn_then_done());
    }

    #[test]
    fn blank_assistant_after_kept_assistant_folds_tool_call_into_kept() {
        let items = vec![user("q"), asst("a"), asst(" "), call("tu_1")];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [
                    {"type": "text", "text": "a"},
                    {"type": "tool_use", "id": "tu_1", "name": "ping", "input": {}},
                ]},
            ])
        );
    }

    #[test]
    fn replayed_blank_user_between_real_turns_is_dropped() {
        let items = vec![user("q"), asst("a"), user(""), user("r")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a"), utext("r")]));
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn trailing_blank_user_after_assistant_sets_flag() {
        let items = vec![user("q"), asst("a"), user("")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(out.trailing_message_dropped);
    }

    #[test]
    fn caller_prefill_does_not_set_flag() {
        let items = vec![user("q"), asst("a"), user(""), asst("b")];
        assert!(!translate_messages(&items).trailing_message_dropped);
    }

    fn tu(id: &str) -> Value {
        json!({"type": "tool_use", "id": id, "name": "ping", "input": {}})
    }

    fn tr(id: &str) -> Value {
        json!({"type": "tool_result", "tool_use_id": id, "content": "pong"})
    }

    fn nested(parts: Vec<ContentPart>) -> Item {
        Item::AssistantMessage {
            content: parts,
            agent: None,
        }
    }

    fn use_part(id: &str) -> ContentPart {
        ContentPart::ToolUse {
            call_id: id.to_owned(),
            name: "ping".to_owned(),
            args: json!({}),
        }
    }

    /// SMA-834: the nested form joins an open round as `Item::ToolCall` does.
    #[test]
    fn nested_tool_use_inside_open_round_joins_it() {
        let items = vec![
            user("q"),
            nested(vec![use_part("t1"), use_part("t2")]),
            result("t1"),
            nested(vec![use_part("t3")]),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1"), tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
            ])
        );
    }

    #[test]
    fn nested_tool_use_after_answered_round_starts_new_round() {
        let items = vec![
            user("q"),
            nested(vec![use_part("t1")]),
            result("t1"),
            nested(vec![use_part("t2")]),
            result("t2"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1")]},
                {"role": "user", "content": [tr("t1")]},
                {"role": "assistant", "content": [tu("t2")]},
                {"role": "user", "content": [tr("t2")]},
            ])
        );
    }

    /// The tool-use-only check reads the blocks after `assistant_blocks`
    /// drops reasoning, so this message joins too.
    #[test]
    fn nested_tool_use_with_reasoning_inside_open_round_joins_it() {
        let items = vec![
            user("q"),
            nested(vec![use_part("t1"), use_part("t2")]),
            result("t1"),
            nested(vec![
                ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                },
                use_part("t3"),
            ]),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1"), tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
            ])
        );
    }

    #[test]
    fn nested_tool_use_with_blank_text_inside_open_round_joins_it() {
        let items = vec![
            user("q"),
            nested(vec![use_part("t1"), use_part("t2")]),
            result("t1"),
            nested(vec![text(" "), use_part("t3")]),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1"), tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
            ])
        );
    }

    #[test]
    fn nested_tool_use_joins_last_of_adjacent_assistant_run() {
        let items = vec![
            user("q"),
            nested(vec![use_part("t1")]),
            nested(vec![use_part("t2")]),
            result("t2"),
            nested(vec![use_part("t3")]),
            result("t1"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1")]},
                {"role": "assistant", "content": [tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t2"), tr("t1"), tr("t3")]},
            ])
        );
    }

    /// A message with text inside an open round still flushes, in both forms.
    /// The test pins today's shape, which the API rejects (known gap, SMA-836).
    /// It also pins the parity between the two forms.
    /// SMA-836 must update the expected array.
    #[test]
    fn mixed_assistant_inside_open_round_matches_top_level_form() {
        let head = vec![
            user("q"),
            nested(vec![use_part("t1"), use_part("t2")]),
            result("t1"),
        ];
        let tail = vec![result("t2"), result("t3")];
        let mut nested_form = head.clone();
        nested_form.push(nested(vec![text("b"), use_part("t3")]));
        nested_form.extend(tail.clone());
        let mut top_level = head;
        top_level.push(asst("b"));
        top_level.push(call("t3"));
        top_level.extend(tail);
        // Pin the shape too: both forms must flush, not join the open round.
        assert_eq!(
            translate_messages(&nested_form).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1"), tu("t2")]},
                {"role": "user", "content": [tr("t1")]},
                {"role": "assistant", "content": [{"type": "text", "text": "b"}, tu("t3")]},
                {"role": "user", "content": [tr("t2"), tr("t3")]},
            ])
        );
        assert_eq!(
            translate_messages(&nested_form).messages,
            translate_messages(&top_level).messages
        );
    }

    /// Adjacent assistant messages are one API turn, so a round counts as
    /// answered only when every call in all of them has a result.
    #[test]
    fn round_spanning_adjacent_assistant_messages_is_not_split() {
        let nested = |id: &str| Item::AssistantMessage {
            content: vec![ContentPart::ToolUse {
                call_id: id.to_owned(),
                name: "ping".to_owned(),
                args: json!({}),
            }],
            agent: None,
        };
        let items = vec![
            user("q"),
            nested("t1"),
            nested("t2"),
            result("t2"),
            call("t3"),
            result("t1"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [tu("t1")]},
                {"role": "assistant", "content": [tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t2"), tr("t1"), tr("t3")]},
            ])
        );
    }

    #[test]
    fn interleaved_parallel_round_is_not_split() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            call("t2"),
            result("t1"),
            call("t3"),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1"), tu("t2"), tu("t3")]},
                {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
            ])
        );
    }

    #[test]
    fn parallel_round_then_second_round_alternates() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            call("t2"),
            result("t1"),
            result("t2"),
            call("t3"),
            result("t3"),
            asst("done"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1"), tu("t2")]},
                {"role": "user", "content": [tr("t1"), tr("t2")]},
                {"role": "assistant", "content": [tu("t3")]},
                {"role": "user", "content": [tr("t3")]},
                atext("done"),
            ])
        );
    }

    #[test]
    fn resume_shape_call_result_call_result() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            result("t1"),
            call("t2"),
            result("t2"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1")]},
                {"role": "user", "content": [tr("t1")]},
                {"role": "assistant", "content": [tu("t2")]},
                {"role": "user", "content": [tr("t2")]},
            ])
        );
    }

    #[test]
    fn trailing_blank_user_then_blank_assistant_sets_flag() {
        let items = vec![user("q"), asst("a"), user(""), asst("")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(out.trailing_message_dropped);
    }

    /// A dropped assistant item inside an open parallel round does not
    /// split the round.
    #[test]
    fn dropped_assistant_inside_open_round_does_not_split_it() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            call("t2"),
            result("t1"),
            asst(""),
            call("t3"),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [
                    {"type": "text", "text": "a"}, tu("t1"), tu("t2"), tu("t3"),
                ]},
                {"role": "user", "content": [tr("t1"), tr("t2"), tr("t3")]},
            ])
        );
    }

    /// A dropped assistant item between a tool result and a user message
    /// does not split the user turn.
    #[test]
    fn dropped_assistant_between_result_and_user_keeps_one_user_turn() {
        let items = vec![
            user("q"),
            asst("a"),
            call("t1"),
            result("t1"),
            asst(" "),
            user("r"),
        ];
        assert_eq!(
            translate_messages(&items).messages,
            json!([
                utext("q"),
                {"role": "assistant", "content": [{"type": "text", "text": "a"}, tu("t1")]},
                {"role": "user", "content": [tr("t1"), {"type": "text", "text": "r"}]},
            ])
        );
    }

    /// A caller prefill that ends with a reasoning-only assistant item is
    /// not a drop-induced prefill: no user turn was dropped.
    #[test]
    fn trailing_reasoning_assistant_without_dropped_user_does_not_set_flag() {
        let items = vec![
            user("q"),
            asst("a"),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn trailing_blank_assistant_without_dropped_user_does_not_set_flag() {
        let items = vec![user("q"), asst("a"), asst("")];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn trailing_blank_user_then_reasoning_assistant_sets_flag() {
        let items = vec![
            user("q"),
            asst("a"),
            user(""),
            Item::AssistantMessage {
                content: vec![ContentPart::Reasoning {
                    text: "scratch".to_owned(),
                }],
                agent: None,
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), atext("a")]));
        assert!(out.trailing_message_dropped);
    }

    #[test]
    fn whitespace_only_system_is_absent() {
        let items = vec![
            Item::System {
                content: vec![text("  \n")],
            },
            user("q"),
        ];
        assert_eq!(translate_messages(&items).system, None);
    }

    #[test]
    fn trailing_tool_result_synthesizes_user_turn() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "ping".to_owned(),
                    args: json!({}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["role"], "user");
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
    }

    #[test]
    fn adjacent_tool_results_coalesce_into_one_user_turn() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![
                    ContentPart::ToolUse {
                        call_id: "tu_a".to_owned(),
                        name: "a".to_owned(),
                        args: json!({}),
                    },
                    ContentPart::ToolUse {
                        call_id: "tu_b".to_owned(),
                        name: "b".to_owned(),
                        args: json!({}),
                    },
                ],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_a".to_owned(),
                content: vec![text("A!")],
            },
            Item::ToolResult {
                call_id: "tu_b".to_owned(),
                content: vec![text("B!")],
            },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["tool_use_id"], "tu_a");
        assert_eq!(blocks[1]["tool_use_id"], "tu_b");
    }

    #[test]
    fn user_with_nested_tool_result_emits_native_block() {
        // The Anthropic-native shape: ContentPart::ToolResult inside a UserMessage.
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::ToolResult {
                call_id: "tu_z".to_owned(),
                content: vec![text("native")],
            }],
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "tu_z");
    }

    #[test]
    fn user_message_with_url_image_emits_url_source() {
        use paigasus_helikon_core::MediaSource;
        let items = vec![Item::UserMessage {
            content: vec![
                text("look:"),
                ContentPart::Image {
                    source: MediaSource::Url {
                        url: "https://example.com/cat.png".to_owned(),
                    },
                },
            ],
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], json!({"type": "text", "text": "look:"}));
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "url");
        assert_eq!(blocks[1]["source"]["url"], "https://example.com/cat.png");
    }

    #[test]
    fn user_message_with_base64_image_emits_base64_source() {
        use paigasus_helikon_core::MediaSource;
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::Image {
                source: MediaSource::Base64 {
                    mime_type: "image/png".to_owned(),
                    data: "AAAA".to_owned(),
                },
            }],
        }];
        let out = translate_messages(&items);
        let block = &out.messages[0]["content"][0];
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "AAAA");
    }
}

/// Provider data from another provider is dropped without a message
/// (SMA-827). This crate has no tracing capture helper, so the tests assert
/// on the output only.
#[cfg(test)]
mod provider_data_tests {
    use super::*;

    fn pd() -> ContentPart {
        ContentPart::ProviderData {
            provider: "gemini".to_owned(),
            call_id: None,
            data: json!({"sig": "SIG_A"}),
        }
    }

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    #[test]
    fn provider_data_dropped_without_warning() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q"), pd()],
            },
            Item::AssistantMessage {
                content: vec![pd()],
                agent: None,
            },
            Item::AssistantMessage {
                content: vec![text("a"), pd()],
                agent: None,
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([
                {"role": "user", "content": [{"type": "text", "text": "q"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "a"}]},
            ]),
        );
        assert!(!out.messages.to_string().contains("SIG_A"));
    }
}

/// SMA-840: the thinking blocks of each assistant message go back in their
/// original order.
#[cfg(test)]
mod thinking_round_trip_tests {
    use super::*;
    use crate::provider_data::{part, AnthropicData, ThinkingBlock};
    use crate::PROVIDER_DATA_KEY;

    const HANDOFF: &str =
        "You are now handling a transferred conversation. Continue assisting the user.";

    fn thinking(i: u32, text: &str, sig: &str) -> ContentPart {
        part(
            AnthropicData::Block {
                index: i,
                block: ThinkingBlock::Thinking {
                    thinking: text.to_owned(),
                    signature: sig.to_owned(),
                },
            },
            None,
        )
    }

    fn redacted(i: u32, data: &str) -> ContentPart {
        part(
            AnthropicData::Block {
                index: i,
                block: ThinkingBlock::RedactedThinking {
                    data: data.to_owned(),
                },
            },
            None,
        )
    }

    fn text_pos(i: u32) -> ContentPart {
        part(AnthropicData::Text { index: i }, None)
    }

    fn tool_pos(i: u32, id: &str) -> ContentPart {
        part(AnthropicData::ToolUse { index: i }, Some(id))
    }

    /// A `ProviderData` part with this crate's key and any `data`.
    fn raw(data: Value) -> ContentPart {
        ContentPart::ProviderData {
            provider: PROVIDER_DATA_KEY.to_owned(),
            call_id: None,
            data,
        }
    }

    fn gemini() -> ContentPart {
        ContentPart::ProviderData {
            provider: "gemini".to_owned(),
            call_id: None,
            data: json!({"sig": "SIG_A"}),
        }
    }

    fn wthinking(text: &str, sig: &str) -> Value {
        json!({"type": "thinking", "thinking": text, "signature": sig})
    }

    fn wredacted(data: &str) -> Value {
        json!({"type": "redacted_thinking", "data": data})
    }

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    fn reasoning(t: &str) -> ContentPart {
        ContentPart::Reasoning { text: t.to_owned() }
    }

    fn user(t: &str) -> Item {
        Item::UserMessage {
            content: vec![text(t)],
        }
    }

    fn asst(parts: Vec<ContentPart>) -> Item {
        Item::AssistantMessage {
            content: parts,
            agent: None,
        }
    }

    fn call(id: &str) -> Item {
        Item::ToolCall {
            call_id: id.into(),
            name: "ping".into(),
            args: json!({}),
        }
    }

    fn result(id: &str) -> Item {
        Item::ToolResult {
            call_id: id.into(),
            content: vec![text("pong")],
        }
    }

    fn use_part(id: &str) -> ContentPart {
        use_part_args(id, json!({}))
    }

    fn use_part_args(id: &str, args: Value) -> ContentPart {
        ContentPart::ToolUse {
            call_id: id.to_owned(),
            name: "ping".to_owned(),
            args,
        }
    }

    fn wtext(t: &str) -> Value {
        json!({"type": "text", "text": t})
    }

    fn tu(id: &str) -> Value {
        json!({"type": "tool_use", "id": id, "name": "ping", "input": {}})
    }

    fn tr(id: &str) -> Value {
        json!({"type": "tool_result", "tool_use_id": id, "content": "pong"})
    }

    fn utext(t: &str) -> Value {
        json!({"role": "user", "content": [wtext(t)]})
    }

    fn amsg(blocks: Vec<Value>) -> Value {
        json!({"role": "assistant", "content": blocks})
    }

    fn umsg(blocks: Vec<Value>) -> Value {
        json!({"role": "user", "content": blocks})
    }

    fn messages(items: &[Item]) -> Value {
        translate_messages(items).messages
    }

    /// The same items with every part of this crate removed.
    fn without_anthropic(items: &[Item]) -> Vec<Item> {
        let keep = |p: &ContentPart| !matches!(p, ContentPart::ProviderData { provider, .. } if provider == PROVIDER_DATA_KEY);
        items
            .iter()
            .map(|item| match item {
                Item::AssistantMessage { content, agent } => Item::AssistantMessage {
                    content: content.iter().filter(|p| keep(p)).cloned().collect(),
                    agent: agent.clone(),
                },
                other => other.clone(),
            })
            .collect()
    }

    #[test]
    fn accumulator_form_restores_wire_order() {
        let items = vec![
            user("q"),
            asst(vec![
                reasoning("r"),
                text("t"),
                thinking(0, "r", "SIG_FAKE_1"),
                text_pos(1),
                tool_pos(2, "tu_1"),
            ]),
            call("tu_1"),
            result("tu_1"),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), wtext("t"), tu("tu_1")]),
                umsg(vec![tr("tu_1")]),
            ])
        );
    }

    #[test]
    fn redacted_blocks_go_back_in_order() {
        let items = vec![
            user("q"),
            asst(vec![
                text("t"),
                redacted(0, "REDACTED_FAKE_1"),
                redacted(1, "REDACTED_FAKE_2"),
                text_pos(2),
                tool_pos(3, "tu_1"),
            ]),
            call("tu_1"),
            result("tu_1"),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![
                    wredacted("REDACTED_FAKE_1"),
                    wredacted("REDACTED_FAKE_2"),
                    wtext("t"),
                    tu("tu_1"),
                ]),
                umsg(vec![tr("tu_1")]),
            ])
        );
    }

    #[test]
    fn interleaved_thinking_keeps_order() {
        let items = vec![
            user("q"),
            asst(vec![
                reasoning("AB"),
                thinking(0, "A", "SIG_FAKE_1"),
                tool_pos(1, "tu_z"),
                thinking(2, "B", "SIG_FAKE_2"),
                tool_pos(3, "tu_a"),
            ]),
            // The accumulator sorts calls by call id.
            call("tu_a"),
            call("tu_z"),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![
                    wthinking("A", "SIG_FAKE_1"),
                    tu("tu_z"),
                    wthinking("B", "SIG_FAKE_2"),
                    tu("tu_a"),
                ]),
            ])
        );
    }

    #[test]
    fn text_without_position_goes_before_first_tool_use() {
        let items = vec![
            user("q"),
            asst(vec![
                text("t"),
                thinking(0, "r", "SIG_FAKE_1"),
                tool_pos(2, "tu_1"),
            ]),
            call("tu_1"),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), wtext("t"), tu("tu_1")]),
            ])
        );
    }

    #[test]
    fn text_without_position_and_no_tool_use_goes_last() {
        let items = vec![
            user("q"),
            asst(vec![text("t"), thinking(5, "r", "SIG_FAKE_1")]),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), wtext("t")]),
            ])
        );
    }

    #[test]
    fn tool_use_without_position_goes_after_keyed_blocks() {
        let head = vec![
            user("q"),
            asst(vec![thinking(0, "r", "SIG_FAKE_1"), tool_pos(1, "tu_1")]),
        ];
        let expected = json!([
            utext("q"),
            amsg(vec![wthinking("r", "SIG_FAKE_1"), tu("tu_1"), tu("tu_2")]),
        ]);
        let mut items = head.clone();
        items.extend([call("tu_1"), call("tu_2")]);
        assert_eq!(messages(&items), expected);
        // The call with no position also goes after a keyed call that
        // arrives later.
        let mut items = head;
        items.extend([call("tu_2"), call("tu_1")]);
        assert_eq!(messages(&items), expected);
    }

    #[test]
    fn repeated_tool_use_id_takes_positions_in_order() {
        let items = vec![
            user("q"),
            asst(vec![
                thinking(2, "r", "SIG_FAKE_1"),
                tool_pos(1, "dup"),
                tool_pos(3, "dup"),
                use_part_args("dup", json!({"n": 1})),
                use_part_args("dup", json!({"n": 2})),
            ]),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![
                    json!({"type": "tool_use", "id": "dup", "name": "ping", "input": {"n": 1}}),
                    wthinking("r", "SIG_FAKE_1"),
                    json!({"type": "tool_use", "id": "dup", "name": "ping", "input": {"n": 2}}),
                ]),
            ])
        );
    }

    #[test]
    fn orphan_position_part_is_ignored() {
        let items = vec![
            user("q"),
            asst(vec![
                text("t"),
                thinking(0, "r", "SIG_FAKE_1"),
                tool_pos(1, "gone"),
            ]),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), wtext("t")]),
            ])
        );
    }

    #[test]
    fn two_digit_indexes_sort_numerically() {
        let items = vec![
            user("q"),
            asst(vec![
                text("t"),
                redacted(10, "REDACTED_FAKE_10"),
                redacted(2, "REDACTED_FAKE_2"),
                text_pos(11),
            ]),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![
                    wredacted("REDACTED_FAKE_2"),
                    wredacted("REDACTED_FAKE_10"),
                    wtext("t"),
                ]),
            ])
        );
    }

    #[test]
    fn mixed_old_and_new_turns() {
        // Turn 1 has the `baseline_accumulator_tool_loop` shape, from an
        // older SDK with no Anthropic part.
        let turn1 = vec![
            user("q"),
            asst(vec![reasoning("r"), text("t")]),
            call("tu_1"),
            result("tu_1"),
            asst(vec![text("done")]),
        ];
        let mut items = turn1.clone();
        items.push(user("q2"));
        items.extend([
            asst(vec![
                reasoning("r2"),
                text("t2"),
                thinking(0, "r2", "SIG_FAKE_1"),
                text_pos(1),
                tool_pos(2, "tu_2"),
            ]),
            call("tu_2"),
            result("tu_2"),
        ]);
        let out = messages(&items);
        let baseline = messages(&turn1);
        let baseline = baseline.as_array().unwrap();
        assert_eq!(&out.as_array().unwrap()[..baseline.len()], &baseline[..]);
        assert_eq!(
            out,
            json!([
                utext("q"),
                amsg(vec![wtext("t"), tu("tu_1")]),
                umsg(vec![tr("tu_1")]),
                amsg(vec![wtext("done")]),
                utext("q2"),
                amsg(vec![wthinking("r2", "SIG_FAKE_1"), wtext("t2"), tu("tu_2")]),
                umsg(vec![tr("tu_2")]),
            ])
        );
    }

    fn earlier_turn_items() -> Vec<Item> {
        vec![
            user("q1"),
            asst(vec![
                text("a1"),
                thinking(0, "r", "SIG_FAKE_1"),
                text_pos(1),
            ]),
            user("q2"),
        ]
    }

    fn earlier_turn_expected() -> Value {
        json!([
            utext("q1"),
            amsg(vec![wthinking("r", "SIG_FAKE_1"), wtext("a1")]),
            utext("q2"),
        ])
    }

    #[test]
    fn earlier_turn_blocks_are_sent_back() {
        assert_eq!(messages(&earlier_turn_items()), earlier_turn_expected());
    }

    #[test]
    fn foreign_provider_data_is_ignored() {
        let items = vec![
            Item::UserMessage {
                content: vec![text("q1"), gemini()],
            },
            asst(vec![
                gemini(),
                text("a1"),
                thinking(0, "r", "SIG_FAKE_1"),
                gemini(),
                text_pos(1),
            ]),
            asst(vec![gemini()]),
            Item::UserMessage {
                content: vec![gemini(), text("q2")],
            },
        ];
        assert_eq!(messages(&items), earlier_turn_expected());
    }

    #[test]
    fn malformed_part_is_skipped() {
        let items = vec![
            user("q1"),
            asst(vec![
                text("a1"),
                raw(json!({"kind": "nope"})),
                raw(json!({"kind": "block", "index": 0})),
                raw(json!({
                    "kind": "block",
                    "index": 1,
                    "block": {"type": "thinking", "thinking": "r", "signature": 5},
                })),
            ]),
            user("q2"),
        ];
        let out = messages(&items);
        assert_eq!(out, messages(&without_anthropic(&items)));
        assert_eq!(
            out,
            json!([utext("q1"), amsg(vec![wtext("a1")]), utext("q2")])
        );
    }

    #[test]
    fn failed_block_check_is_skipped() {
        let items = vec![
            user("q1"),
            asst(vec![
                text("a1"),
                thinking(0, "r", ""),
                redacted(1, ""),
                text_pos(2),
            ]),
            user("q2"),
        ];
        assert_eq!(
            messages(&items),
            json!([utext("q1"), amsg(vec![wtext("a1")]), utext("q2")])
        );
    }

    #[test]
    fn block_is_rebuilt_from_known_fields() {
        let items = vec![
            user("q"),
            asst(vec![
                text("a"),
                raw(json!({
                    "kind": "block",
                    "index": 0,
                    "block": {
                        "type": "thinking",
                        "thinking": "r",
                        "signature": "SIG_FAKE_1",
                        "cache_control": {"type": "ephemeral"},
                    },
                })),
                text_pos(1),
            ]),
        ];
        let out = messages(&items);
        assert_eq!(
            out,
            json!([
                utext("q"),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), wtext("a")]),
            ])
        );
        assert_eq!(
            out[1]["content"][0].as_object().unwrap().len(),
            3,
            "only type, thinking and signature",
        );
    }

    #[test]
    fn sma834_join_with_thinking_both_forms() {
        let nested_form = vec![
            user("q"),
            asst(vec![
                thinking(0, "T1", "SIG_FAKE_1"),
                tool_pos(1, "t1"),
                tool_pos(2, "t2"),
                use_part("t1"),
                use_part("t2"),
            ]),
            result("t1"),
            asst(vec![
                thinking(0, "T2", "SIG_FAKE_2"),
                tool_pos(1, "t3"),
                use_part("t3"),
            ]),
            result("t2"),
            result("t3"),
        ];
        // The form that the accumulator writes (ledger ruling F4).
        let accumulator_form = vec![
            user("q"),
            asst(vec![
                reasoning("T1"),
                thinking(0, "T1", "SIG_FAKE_1"),
                tool_pos(1, "t1"),
                tool_pos(2, "t2"),
            ]),
            call("t1"),
            call("t2"),
            result("t1"),
            asst(vec![
                reasoning("T2"),
                thinking(0, "T2", "SIG_FAKE_2"),
                tool_pos(1, "t3"),
            ]),
            call("t3"),
            result("t2"),
            result("t3"),
        ];
        let expected = json!([
            utext("q"),
            amsg(vec![
                wthinking("T1", "SIG_FAKE_1"),
                tu("t1"),
                tu("t2"),
                wthinking("T2", "SIG_FAKE_2"),
                tu("t3"),
            ]),
            umsg(vec![tr("t1"), tr("t2"), tr("t3")]),
        ]);
        assert_eq!(messages(&nested_form), expected);
        assert_eq!(messages(&accumulator_form), expected);
    }

    #[test]
    fn handoff_transcript_thinking_only_message_is_dropped() {
        let items = vec![
            user("q"),
            asst(vec![
                reasoning("r"),
                thinking(0, "r", "SIG_FAKE_1"),
                tool_pos(1, "tu_1"),
            ]),
            user(HANDOFF),
        ];
        let out = translate_messages(&items);
        let base = translate_messages(&without_anthropic(&items));
        assert_eq!(out.messages, base.messages);
        assert_eq!(out.messages, json!([utext("q"), utext(HANDOFF)]));
        assert_eq!(out.trailing_message_dropped, base.trailing_message_dropped);
    }

    #[test]
    fn reasoning_only_assistant_is_not_sent_with_anthropic_blocks() {
        let items = vec![
            user("q"),
            asst(vec![
                reasoning("scratch"),
                thinking(0, "scratch", "SIG_FAKE_1"),
            ]),
            user("r"),
        ];
        assert_eq!(messages(&items), json!([utext("q"), utext("r")]));
    }

    #[test]
    fn trailing_blank_user_then_reasoning_assistant_sets_flag_with_anthropic_blocks() {
        let items = vec![
            user("q"),
            asst(vec![text("a")]),
            user(""),
            asst(vec![
                reasoning("scratch"),
                thinking(0, "scratch", "SIG_FAKE_1"),
            ]),
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q"), amsg(vec![wtext("a")])]));
        assert!(out.trailing_message_dropped);
    }

    #[test]
    fn length_finish_after_thinking_is_dropped() {
        let items = vec![
            user("q"),
            asst(vec![reasoning("r"), thinking(0, "r", "SIG_FAKE_1")]),
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages, json!([utext("q")]));
        assert!(!out.trailing_message_dropped);
    }

    #[test]
    fn transfer_split_moves_tool_use_unkeyed() {
        let items = vec![
            user("q"),
            asst(vec![
                reasoning("r"),
                thinking(0, "r", "SIG_FAKE_1"),
                tool_pos(1, "a"),
                tool_pos(2, "b"),
            ]),
            call("a"),
            result("a"),
            call("b"),
            result("b"),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), tu("a")]),
                umsg(vec![tr("a")]),
                amsg(vec![tu("b")]),
                umsg(vec![tr("b")]),
            ])
        );
    }

    /// Calls that a split moves into a later wire message lose their keys,
    /// so they keep today's order there (here `b` before `c`, although the
    /// positions put `c` first).
    #[test]
    fn moved_tool_uses_keep_todays_order() {
        let items = vec![
            user("q"),
            asst(vec![
                reasoning("r"),
                thinking(0, "r", "SIG_FAKE_1"),
                tool_pos(1, "a"),
                tool_pos(3, "b"),
                tool_pos(2, "c"),
            ]),
            call("a"),
            result("a"),
            call("b"),
            call("c"),
            result("b"),
            result("c"),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), tu("a")]),
                umsg(vec![tr("a")]),
                amsg(vec![tu("b"), tu("c")]),
                umsg(vec![tr("b"), tr("c")]),
            ])
        );
    }

    /// Ruling F7: the thinking-only rule reads the wire message. Here the
    /// call of the source message lands in a later wire message, so the wire
    /// message of the thinking block has no `tool_use` block. The block is
    /// dropped and the request is the same as with no Anthropic part. A
    /// look-ahead over the source message only would keep the block.
    #[test]
    fn thinking_only_message_whose_call_lands_elsewhere_is_dropped() {
        let items = vec![
            user("q"),
            asst(vec![text("a")]),
            call("t1"),
            asst(vec![
                reasoning("r"),
                thinking(0, "r", "SIG_FAKE_1"),
                tool_pos(1, "t2"),
            ]),
            result("t1"),
            call("t2"),
            result("t2"),
        ];
        let out = messages(&items);
        assert_eq!(out, messages(&without_anthropic(&items)));
        assert_eq!(
            out,
            json!([
                utext("q"),
                amsg(vec![wtext("a"), tu("t1")]),
                umsg(vec![tr("t1")]),
                amsg(vec![tu("t2")]),
                umsg(vec![tr("t2")]),
            ])
        );
    }

    /// Ruling F7, the other direction: the source message has no call, but a
    /// later nested `tool_use` joins its wire message. The wire message then
    /// has a `tool_use` block, so the thinking block stays.
    #[test]
    fn thinking_only_message_joined_by_later_tool_use_is_kept() {
        let items = vec![
            user("q"),
            asst(vec![text("a")]),
            call("t1"),
            call("t2"),
            asst(vec![reasoning("r"), thinking(0, "r", "SIG_FAKE_1")]),
            result("t1"),
            asst(vec![use_part("t3")]),
            result("t2"),
            result("t3"),
        ];
        assert_eq!(
            messages(&items),
            json!([
                utext("q"),
                amsg(vec![wtext("a"), tu("t1"), tu("t2")]),
                amsg(vec![wthinking("r", "SIG_FAKE_1"), tu("t3")]),
                umsg(vec![tr("t1"), tr("t2"), tr("t3")]),
            ])
        );
    }

    /// A dropped thinking-only message does not change the system text or
    /// the items around it.
    #[test]
    fn dropped_thinking_only_message_keeps_system_text_once() {
        let items = vec![
            Item::System {
                content: vec![text("s1")],
            },
            user("q"),
            asst(vec![reasoning("r"), thinking(0, "r", "SIG_FAKE_1")]),
            Item::System {
                content: vec![text("s2")],
            },
            user("r"),
        ];
        let out = translate_messages(&items);
        assert_eq!(out.system, Some(Value::String("s1\ns2".to_owned())));
        assert_eq!(out.messages, json!([utext("q"), utext("r")]));
        assert!(!out.trailing_message_dropped);
    }
}

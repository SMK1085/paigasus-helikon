# SMA-819: drop empty text blocks in the Anthropic translator

Status: revised after the spec challenge, 2026-11-20. Size: a small change.

## Intent

The Anthropic translator (`crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`)
sends `{"type": "text", "text": ""}` for an empty user text part. The Messages
API probably rejects empty text blocks with HTTP 400. The live check must
confirm this. The bad item stays in the session, so each replay of that
session fails again.

These paths make a blank text part:

- an empty user input;
- a turn-boundary checkpoint that returns `Inject([Text ""])` (PR #346);
- whitespace-only text that a model emits, which is then replayed;
- a session that another provider recorded.

## Goals

- The translator drops each `ContentPart::Text` whose `text.trim()` is empty.
  The definition of "blank" is Rust `str::trim()` (Unicode `White_Space`). The
  design accepts any difference from the API's own definition. This rule applies
  to user messages and assistant messages, new and replayed.
- The translator does not send a user message or an assistant message that has
  no blocks left. This is wider than blank text. It also removes messages that
  have only `Reasoning`, only `Audio`, or only unknown parts. Today those go out
  as `content: []`.
- The translator sends `tool_use` and `tool_result` blocks in the order of the
  items. The message that removes an empty assistant carrier needs this fix (see
  "Ordering fix").
- The system text is treated as absent when `system_text.trim()` is empty. This
  is a one-line change at `request.rs:106`. It prevents a whitespace-only system
  block with `cache_control`.
- `build_body` returns `ModelError::Other` before any HTTP call when the
  translated `messages` array is empty. The error text is neutral, for example
  "no message with content to send (empty and whitespace-only text is not
  sent)". The text must also be correct for a request that has only
  `Item::System`.

## Non-goals

- No change to `tool_result` content. The live check records whether the API
  accepts an empty `tool_result` string, for information only.
- No change to the other providers. The Gemini translator has a related problem
  in SMA-820. The Bedrock translator
  (`crates/paigasus-helikon-providers-bedrock/src/translate/request.rs`) also
  emits blank text. SMA-824 covers it.
- No change to `paigasus-helikon-core`. Items in the session stay as they are.
  The fix is on the wire only.
- No support for extended thinking across a tool loop. The translator drops
  `Reasoning` on input today, and this ticket does not change that.

## Anti-goals

- The translator must not put placeholder text in place of a dropped block. That
  would send words the user did not write.
- The translator must not block a final assistant message that a caller sends on
  purpose (a prefill).
- The fix must not make a request fail that works on `main` today.

## Decisions (from the conversation)

1. **Edge case (Sven chose option c, after the spec challenge).** `build_body`
   fails early with `ModelError::Other` in two cases:
   - the drops leave no messages;
   - the last conversation item (`UserMessage`, `AssistantMessage`, `ToolCall`
     or `ToolResult`) is a `UserMessage` that the translator dropped, and the
     request now ends with an assistant message.

   A caller that ends the item list with an assistant message on purpose (a
   prefill) is not blocked. The translator reports the second case to
   `build_body`, for example with a flag on `TranslatedMessages`. Sven first
   chose option (b) and changed to (c), because a drop-induced prefill makes
   the model continue its old answer.
2. **Approach (Sven chose option 1).** Filter where the blocks are made. See
   "Approaches".

## Approaches

1. **Filter where the blocks are made (chosen).** `user_blocks` and
   `assistant_blocks` skip blank `Text` parts. The `UserMessage` and
   `AssistantMessage` arms do not push a message with no blocks. `build_body`
   checks for an empty `messages` array. The rule stays next to the code that
   made the problem.
2. **A cleanup pass on the finished JSON.** This catches blocks that other code
   adds later. But it reads untyped JSON and puts the rule away from the code
   that makes the blocks.
3. **Filter the `Item` list before translation.** This puts an Anthropic rule in
   a generic place and must rebuild `Item` values. It does not fit.

## Ordering fix

Today an `AssistantMessage` with zero blocks is still pushed as
`assistant: []`. A following `Item::ToolCall` folds into it. That hides an
ordering bug. When this change removes the empty carrier, the bug shows:

```text
items: User q, Assistant[Reasoning], ToolCall tu_1, ToolResult tu_1, Assistant "done"
bad:   user[q], user[tool_result tu_1], assistant[tool_use tu_1], assistant[done]
good:  user[q], assistant[tool_use tu_1], user[tool_result tu_1], assistant[done]
```

The same bug is on `main` today for a tool call that has no `AssistantMessage`
(`User q, ToolCall, ToolResult, Assistant "done"`). A second form is also on
`main`: `ToolCall t2` after a pending `ToolResult r1` folds into the earlier
assistant message, before `r1`.

The fix has three parts. Together they make the wire order follow the item
order:

1. The `AssistantMessage` arm flushes pending `tool_use` blocks before pending
   `tool_result` blocks. The `System` arm and the end of the loop already use
   this order.
2. The `ToolCall` arm flushes pending `tool_result` blocks before the fold
   check.
3. The `ToolResult` arm flushes pending `tool_use` blocks first.

## Behavior details

- A `UserMessage` that has pending `tool_result` blocks keeps those blocks when
  all its text is blank. The message is sent with only the `tool_result` blocks.
  This covers the SMA-804 `Inject([Text ""])` path after a tool result.
- When the translator drops an `AssistantMessage`, a following `Item::ToolCall`
  folds into the last message if that message is an assistant message, and
  goes into `pending_tool_use` if it is not. The ordering fix keeps both cases
  correct.
- A dropped message can put two messages with the same role next to each other.
  The translator already sends two adjacent user messages for two adjacent
  `UserMessage` items. The design keeps that behavior and relies on the API to
  join the turns. The live check must confirm this.
- A text block that is not blank keeps its exact text, with its whitespace. The
  translator does not trim the text it sends.
- `cache::apply_cache_strategy` runs after the drops, so the `LastTurn` marker
  goes on a block that is really sent.

## Verification

### Live probes (before the code change)

The probes need a real key: `ANTHROPIC_API_KEY`, or Claude through Bedrock or
Vertex. A keyless proxy with `mock_response` does not reach Anthropic's
validation, so it cannot prove these claims. **If no real key is available,
stop and bring this back to Sven.** Record the request bodies and the
responses. Never record headers.

Use `claude-haiku-4-5` and `claude-sonnet-4-6` (the IDs in `tests/live.rs`),
with thinking off unless the probe says otherwise. The probe bodies are:

| # | `messages` | What it proves |
|---|------------|----------------|
| P1 | `[user [text ""]]` | the 400 for an empty block |
| P2 | `[user [text "  \n\t"]]` | the 400 for a whitespace-only block |
| P3 | `[user "a", user "b"]` | the API joins adjacent user turns |
| P4 | `[user "q", assistant "a1", assistant "a2", user "r"]` | the API joins adjacent assistant turns in the middle |
| P5 | `[user "q", assistant "Sure."]` | a final assistant message (a prefill) works |
| P6 | P5 with thinking on | a prefill with thinking |
| P7 | `[user "q", assistant "Sure.\n"]` | a prefill with trailing whitespace |
| P8 | `[assistant "a"]` | an assistant-first request |
| P9 | a `tool_result` with content `""` | information only (non-goal) |

**Stop conditions.** If P3 or P4 gets a 400, stop and bring it back to Sven
before you change the design. If P5, P6, P7 or P8 gets a 400, record it and
report it at the next gate. Decision (c) already blocks a drop-induced
prefill, so these results are for the record.

### Live tests (after the fix)

Add `#[ignore]` tests to `tests/live.rs`. Record the output of a real run in
the PR. A skipped run does not count. The cases are:

- (a) user, assistant with `ToolCall`, `ToolResult`, blank user. This is the
  SMA-804 path.
- (b) case (a) with `CacheStrategy::LastTurn`.
- (c) user, reasoning-only assistant, `ToolCall`, `ToolResult`, assistant text,
  user. This is the ordering path.
- (d) user, assistant, blank user, real user. This is the replay path.

### Unit tests in `translate/request.rs`

Compare the full `messages` array with `assert_eq!`, not only lengths.

- `empty_text_part_is_emitted_as_empty_block` changes to assert that the block
  is dropped. Rename it to match.
- Whitespace-only text (`"  \n\t"`) is dropped.
- A user message with no blocks left is not sent.
- A user message with blank text and pending tool results keeps the
  `tool_result` blocks.
- Blank assistant text is dropped, and an assistant message with no blocks left
  is not sent.
- A reasoning-only assistant message is not sent.
- A replayed sequence (real user, assistant, blank user, real user) keeps only
  the real messages.
- Ordering: user, reasoning-only assistant, `ToolCall`, `ToolResult`, assistant
  text.
- Ordering: the same with whitespace assistant text.
- Ordering: a tool call with no `AssistantMessage`, then a `ToolResult`, then
  assistant text. This pins the bug that is on `main`.
- Ordering: a `ToolCall` after a pending `ToolResult` does not fold into the
  earlier assistant message.
- Fold target: a blank assistant after a kept assistant, then a `ToolCall`.
- A whitespace-only system text gives no `system` field.

### Tests for `build_body` and `invoke`

- `build_body` with only a blank user message returns `ModelError::Other`.
- `build_body` with only `Item::System` returns `ModelError::Other`.
- `build_body` with `Item::System` and a blank user message returns
  `ModelError::Other`.
- `build_body` with `[user "q", assistant "a", user ""]` returns
  `ModelError::Other` (decision c).
- `build_body` with `[user "q", assistant "a", user "", assistant "b"]` sends
  the request. The caller ended the list with an assistant message.
- `build_body` with `[user "q", assistant "a", ToolCall, ToolResult, user ""]`
  sends the request. The `tool_result` blocks make a user message.
- `build_body` with `CacheStrategy::LastTurn` and a trailing blank user message
  puts the marker on the previous real block, and no block has blank text.
- A wiremock test through `invoke` with `.expect(0)`: a blank-only request
  returns `Err` and sends no HTTP request.

## Settled at Gate 1

1. Decision 1 is option (c). See "Decisions".
2. All three parts of the ordering fix are in SMA-819. Two parts are on `main`
   today, but the change needs them to avoid a regression, and they have the
   same cause.
3. The Bedrock translator gets a separate Linear issue.
4. The live probes use `ANTHROPIC_API_KEY` from the repo-root `.env`. Load it
   into the process environment only. Never print it or write it to a file.

## Left to the builder

- The exact error text, within the meaning given above.
- The name of the helper that checks for blank text.
- Whether the translator emits a `tracing::debug!` line when it drops a block.
  If it does, the line contains counts only, never text.

## Documentation

- The crate `README.md` and the mdBook do not describe this translation rule.
  Check both. If nothing describes it, no edit is necessary.
- The fix is a `fix(providers)` change, so release-plz bumps the Anthropic crate
  and the facade as usual. No hand version bump.

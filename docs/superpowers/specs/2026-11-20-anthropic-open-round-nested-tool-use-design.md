# SMA-834: Anthropic translator keeps an open tool round whole at a tool-use-only assistant message

Linear: [SMA-834](https://linear.app/smaschek/issue/SMA-834/anthropic-translator-splits-an-open-tool-round-at-a-tool-use-only)

## Intent

The core model has two equal forms for a tool call: a top-level
`Item::ToolCall`, and a `ContentPart::ToolUse` nested inside an
`Item::AssistantMessage`. In
`crates/paigasus-helikon-providers-anthropic/src/translate/request.rs` the two
forms give different wire orders inside an open tool round. The nested form
splits the round, and the Anthropic API rejects the request because a user turn
lacks a `tool_result` for a `tool_use` in the turn before it.

SMA-824 fixed the same gap in the Bedrock translator (PR #365, squash commit
`4003a131` on `main`). This change ports that rule.

## Current behavior

- The `ToolCall` arm keeps the pending results when `round_is_answered` is
  `false`. It appends its `tool_use` block to the last assistant message in
  `messages`, or to `pending_tool_use` when the last message is not an
  assistant message.
- The `AssistantMessage` arm, for a non-empty message, always flushes
  `pending_tool_use` and `pending_tool_results`, then pushes a new assistant
  message.

Example items: `user q, assistant[ToolUse t1, ToolUse t2], ToolResult t1,
assistant[ToolUse t3], ToolResult t2, ToolResult t3`.

- Today: `user[q], assistant[t1, t2], user[r1], assistant[t3], user[r2, r3]`.
  The API rejects this.
- With the fix: `user[q], assistant[t1, t2, t3], user[r1, r2, r3]`. This is the
  same wire as the `Item::ToolCall t3` form.

## Goals

- A non-empty `AssistantMessage` joins the open round when all three conditions
  are true:
  1. every block in the **output of `assistant_blocks`** is a `tool_use` block.
     `assistant_blocks` already drops blank text, reasoning, and unsupported
     parts, so `[Reasoning, ToolUse t3]` and `[Text " ", ToolUse t3]` join.
     This matches Bedrock.
  2. `pending_tool_results` is not empty,
  3. `round_is_answered(&messages, &pending_tool_results)` is `false`.
- "Joins" means the same thing as in the `ToolCall` arm: the pending results stay
  pending, and the blocks go to the end of the last assistant message in
  `messages`. One private helper does "append to the last assistant message, or
  else queue in `pending_tool_use`". The `ToolCall` arm and the new branch both
  call it, so the two paths cannot become different.
- Two invariants make the append correct. The code comment states both:
  - On the join path the last message is an assistant message.
    `round_is_answered` returns `true` for an empty trailing assistant run, so
    condition 3 needs at least one assistant message at the end.
  - On the join path `pending_tool_use` is empty. It gets blocks only when the
    last message is not an assistant message, and each `ToolResult` flushes it.
    Condition 2 needs a `ToolResult` after the last queue.
- `last_item_dropped` becomes `false` on that path, for symmetry with every
  kept message. No test can observe it: on the join path the flag is already
  `false`, because a `ToolResult` came after any dropped user message.
- Assumption: an assistant message inside an open round belongs to the same
  model response. The `ToolCall` arm already makes this assumption
  (`interleaved_parallel_round_is_not_split`). For a log with a late result,
  the fix sends a valid request in a different order instead of a rejected one.

## Non-goals

- No change for an answered round, for no pending results, or for an empty
  message. These keep their current behavior.
- **Known gap, kept as is:** a text-only or mixed (text plus tool use) assistant
  message inside an open round still flushes. The nested form and the
  top-level form (`AssistantMessage(text)` then `ToolCall`, as `build_items`
  emits) both give a request that the API rejects. Bedrock has the same gap
  (`assistant_message_flushes_calls_before_results`). This goes to a follow-up
  issue for both translators.
- No de-duplication of tool-use ids. `asst[t1, t2], r1, asst[tu t1]` gives
  `asst[t1, t2, t1]`, which the API rejects. Today's output is also rejected,
  and the `ToolCall` arm does not de-duplicate either.
- No change to `round_is_answered`, `user_blocks`, or `assistant_blocks`.
- No change to other providers. Bedrock is already fixed. The OpenAI, LiteLLM,
  and Gemini translators are out of scope.

## Anti-goals

- No change to the wire for a request that the API accepts today. The new
  branch runs only when today's output already has a user turn that lacks a
  result. The existing tests stay green without edits, including
  `round_spanning_adjacent_assistant_messages_is_not_split` (two adjacent
  nested messages with no pending results stay two messages).

## Approaches

1. **Port the Bedrock condition into the `AssistantMessage` arm, with a shared
   append helper (chosen).** One extra branch before the current flush. It
   matches Bedrock and the `ToolCall` arm, and it is the smallest change.
2. **Turn a tool-use-only message into `Item::ToolCall` items and reuse that
   arm.** This needs a refactor of the loop for no gain, and it makes the two
   translators differ in shape.
3. **Merge every tool-use-only message into the previous assistant message.**
   This changes the wire for well-formed requests (for example, two adjacent
   nested messages), so it breaks the anti-goal.

## Tests

Unit tests in the `tests` module of `request.rs`, with Anthropic wire arrays:

- `nested_tool_use_inside_open_round_joins_it`: the example above gives
  `[utext("q"), assistant[tu(t1), tu(t2), tu(t3)], user[tr(t1), tr(t2), tr(t3)]]`.
- `nested_tool_use_after_answered_round_starts_new_round`:
  `user q, assistant[t1], ToolResult t1, assistant[t2], ToolResult t2` gives
  `[utext("q"), assistant[tu(t1)], user[tr(t1)], assistant[tu(t2)], user[tr(t2)]]`.
- `nested_tool_use_with_reasoning_inside_open_round_joins_it`: the first example
  with `[Reasoning, ToolUse t3]` gives the same array. This proves condition 1
  reads the filtered blocks.
- `nested_tool_use_joins_last_of_adjacent_assistant_run`: a copy of
  `round_spanning_adjacent_assistant_messages_is_not_split` with a nested `t3`
  in place of `call("t3")`. The expected array is the same as in that test.
- `mixed_assistant_inside_open_round_matches_top_level_form`: a parity test.
  The nested form `asst[text b, tu t3]` and the top-level form
  `asst(text b), ToolCall t3`, each inside an open round, give equal output.
  The test asserts the parity, not a specific array. Its doc comment says that
  both forms still give a rejected request (the known gap). Without condition 1
  this test fails.

Live test: `nested_tool_use_inside_open_round_is_accepted` in
`crates/paigasus-helikon-providers-anthropic/tests/live.rs`, `#[ignore]`, with
`run_accepted` and `toolu_...` ids, as the SMA-819 live tests do. If an API key
is available, one real run goes into the PR body.

## Docs

Internal translation fix. The public API, the mdBook, and the crate README do
not change. The code comment of the `AssistantMessage` arm explains the new
branch and the two invariants, as Bedrock's comment does.

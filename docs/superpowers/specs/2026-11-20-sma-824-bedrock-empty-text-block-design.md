# SMA-824: drop empty text blocks in the Bedrock translator

Status: approved at Gate 1, 2026-11-20. Size: a small change.

## Intent

The Bedrock Converse translator
(`crates/paigasus-helikon-providers-bedrock/src/translate/request.rs`) sends
`ContentBlock::Text` for each `ContentPart::Text`, also when the text is blank.
Converse rejects blank text blocks (probes B1, B2, B7 below). The bad item stays
in the session, so each replay of that session fails again.

This is the Bedrock form of SMA-819 (PR #353), which fixed the Anthropic
translator. The rules come from the SMA-819 design
(`docs/superpowers/specs/2026-11-20-sma-819-anthropic-empty-text-block-design.md`).

These paths make a blank text part: an empty user input, a turn-boundary
checkpoint that returns `Inject([Text ""])`, whitespace-only text that a model
emits and that is then replayed, and a session that another provider recorded.

## How the Bedrock translator differs from the Anthropic one

- The `UserMessage` and `AssistantMessage` arms already skip a message that has
  no blocks.
- The translator already joins adjacent same-role turns. Converse also accepts
  adjacent same-role turns (probes B3, B9). The doc comments that say
  "strictly alternating" are thus too strict (see "Doc sites").
- The translator adds a placeholder user turn `Text("")` when the first message
  is an assistant message. The service rejects that block (probe B11 on Claude;
  B1 shows the same service-level error on Nova Lite and Pixtral Large). Thus,
  on `main`, each request that starts with an assistant message fails.
- On `main`, the `AssistantMessage` arm flushes pending tool results before
  pending tool calls. On `main` this order is correct, because both lists are
  pending at the same time only when a `ToolCall` comes after a `ToolResult`.
  After the `round_is_answered` port (below), both lists are pending only when
  the round is still open. Then "results first" puts late calls into a new
  assistant turn after partial results, while the other arms put them into the
  open round. The fix changes this arm to "calls first", which matches the other
  Bedrock arms and SMA-819.
- The translator has the second ordering bug from SMA-819. A `ToolCall` that
  comes after a pending `ToolResult` folds into the earlier assistant message.
  The items `assistant[a, t1], ToolResult r1, ToolCall t2, ToolResult r2` become
  `assistant[a, t1, t2], user[r1, r2]`. Converse accepts this, but the history
  is wrong: the model called `t2` after it saw `r1`. The fold happens at each
  site that calls `flush_assistant` while results are pending: the `ToolResult`
  arm, the `UserMessage` arm, the `System` arm and the end of the input. At the
  last three sites, `t2` goes before `user[r1]`.

## Goals

- **Blank text.** The translator drops each `ContentPart::Text` whose
  `text.trim()` is empty, in user messages and in assistant messages. "Blank"
  means Rust `str::trim()` (Unicode `White_Space`). A text that is not blank
  keeps its exact text, with its whitespace.
- **Blank system text.** An `Item::System` whose text is blank pushes no
  `SystemContentBlock` (probe B12). It still flushes the pending turns, as today.
  Compaction can put `Item::System` in the middle of a conversation
  (`crates/paigasus-helikon-core/src/session.rs:741`), so the flush must stay.
- **No empty messages.** The translator does not send a message that has no
  blocks left. (This is already true. The change must keep it.)
- **An empty assistant item is absent.** An `AssistantMessage` with no blocks
  left flushes nothing and does not clear the drop flag. This is the SMA-819
  rule. Commit this change in the same commit as the `round_is_answered` port,
  because the reasoning-only tool loop is correct only with both changes.
- **Tool order.** The `ToolCall` arm flushes pending tool results before it
  folds a call into the last assistant message, when the last assistant round is
  fully answered. This ports `round_is_answered` from SMA-819. The
  `AssistantMessage` arm flushes calls before results.
- **The placeholder is removed (Sven chose option 1).** The translator sends a
  request that starts with an assistant message as it is. Claude models accept
  it (probe B4). Nova and Mistral reject it with the server error "A
  conversation must start with a user message".
- **Early errors.** `items_to_messages` returns `ModelError::Other` before any
  network call in two cases (the SMA-819 rule, option c):
  1. no message remains;
  2. the last conversation item (`UserMessage`, `AssistantMessage`, `ToolCall`
     or `ToolResult`) is a `UserMessage` that the blank-text rule dropped, and
     the request now ends with an assistant message. An `AssistantMessage` that
     produced no blocks after that user message does not count as a later item.
     A `ToolCall` or `ToolResult` does count, and so does a kept message.

  **"Dropped" for case 2** means: the `UserMessage` has at least one blank
  `Text` part, and it produced no block. A `UserMessage` with `content: []`, or
  with only `Image` or `Audio` parts, keeps the behavior of `main`. It produces
  no message, and it does not set the flag. This keeps those requests as they
  are today.

  Each case has its own error text, and each text names the cause. Case 1 says
  that no message has content to send, because empty, whitespace-only and
  unsupported parts are not sent. It must also be correct for a request with
  only `Item::System`. Case 2 says that the final user message had only blank
  text, so the request would end with an assistant message. The two texts in
  `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs` (lines 37
  and 42) are the model.

## Non-goals

- No change to `tool_result` content. Converse accepts an empty `tool_result`
  text (probe B8).
- No change to the other providers and no change to `paigasus-helikon-core`.
  The fix is on the wire only.
- No early error for families that need a user message first. The server error
  is already clear.
- **Known limitation.** On Nova and Mistral, a session whose first user message
  is blank and is followed by an assistant reply still cannot be replayed. An
  example is `[user "", assistant "Hi", user "x"]`. Today this fails with the
  B1 error. After the fix it fails with "A conversation must start with a user
  message". A compaction that leaves an assistant message first has the same
  result.
- A request with only assistant messages, for example `[user "", assistant a]`
  or `[ToolCall t1]`, goes out as it is. Sonnet 5.5 rejects it as a prefill
  (B5). This is not a regression, because the placeholder makes it fail on
  `main` too.

## Anti-goals

- The translator must not put placeholder text in place of a dropped block. That
  would send words the user did not write.
- The translator must not block a final assistant message that a caller sends on
  purpose (a prefill).
- The fix must not make a request fail that works on `main` today. **Exception
  (accepted at Gate 1):** see "Settled at Gate 1".

## Settled at Gate 1

Sven approved the spec on 2026-11-20 and chose option 1 below: one blank-text
rule for all families, with the two shapes below as an accepted exception to
the anti-goal.

Nova Lite and Pixtral Large accept whitespace-only text (probes B2, B13, B14).
Thus the blank-text rule makes two request shapes fail on those families where
they return 200 today:

- a whitespace-only user message alone, for example `[user "  \n\t"]` (case 1);
- a whitespace-only final user message after an assistant message, for example
  `[user q, assistant a, user "  "]` (case 2).

The model answers to these requests contain no useful content (B13 on Nova
Lite: "It looks like you provided an image rather than a question").

The choices:

1. **Accept the change (chosen).** One rule for all families. The two shapes
   are an accepted exception to the anti-goal.
2. **Drop whitespace-only text only for Anthropic models, and empty text for all
   families.** This keeps the anti-goal. But family detection is not reliable:
   `global.anthropic.claude-sonnet-5-5` is detected as `ModelFamily::Unknown`
   (see "Found during the spec challenge"). Thus this choice needs that fix
   first, or Claude requests through a `global.` profile keep the bug.

## Approaches

1. **Filter where the blocks are made (chosen).** The `UserMessage` and
   `AssistantMessage` arms skip blank `Text` parts, and the `System` arm skips
   blank text. The rule stays next to the code that made the problem. This is
   the SMA-819 approach.
2. **A cleanup pass on the finished `Vec<Message>`.** This catches blocks that
   other code adds later. But it puts the rule away from the code that makes the
   blocks, and it cannot see the item order that the edge cases need.
3. **Filter the `Item` list before translation.** This puts a provider rule in a
   generic place and must rebuild `Item` values. It does not fit.

## Behavior details

- A `UserMessage` that has pending tool results keeps those blocks when all its
  text is blank. The message is sent with only the `ToolResult` blocks. This is
  the SMA-804 `Inject([Text ""])` path after a tool result. It does not set the
  flag.
- The prefill error (probe B5) is not the translator's concern for a caller
  prefill. Only the drop-induced case fails early.
- Non-obvious expected arrays:
  - `[user q, assistant a, ToolCall t1, ToolResult r1, ToolCall t2]` (end of
    input) gives `[user[q], assistant[a, t1], user[r1], assistant[t2]]`.
  - The same items followed by `Item::System "s"` give the same messages, and
    the system slot has `s`.
  - `[user "", assistant a, user b]` gives `[assistant[a], user[b]]`.

## Doc sites

These comments become false and change with the code:

- `request.rs:1-13`, the module doc: the "first message must be `user`" rule
  goes, "strictly alternating" changes, and the blank-text rule comes in.
- `request.rs:31`, the `messages` field doc ("strictly alternating").
- `request.rs:35-38`, the `items_to_messages` doc: add the two error cases.
- `translate/mod.rs:33` ("strictly alternating") and the `build_request`
  `# Errors` list (`translate/mod.rs:85-89`): add the two error cases.

## Verification

### Live probes (before the code change, 2026-11-20)

Region `eu-central-1`. The default model is
`global.anthropic.claude-sonnet-5-5`. The request bodies are in the table.
Headers were not recorded.

| # | `messages` (and `system`) | Claude Sonnet 5.5 | Nova Lite | Pixtral Large |
|---|---------------------------|-------------------|-----------|---------------|
| B1 | `[user ""]` | 400 `The text field in the ContentBlock object at messages.0.content.0 is blank.` | 400, same | 400, same |
| B2 | `[user "  \n\t"]` | 400 `messages: text content blocks must contain non-whitespace text` | 200 | 200 |
| B3 | `[user "Say hi.", user "Shortly."]` | 200 | 200 | 200 |
| B4 | `[assistant "Hello.", user "Say hi."]` | 200 | 400 `A conversation must start with a user message.` | 400, same |
| B5 | `[user "Name a color.", assistant "Sure."]` | 400 `This model does not support assistant message prefill.` | 200 | 400 `requires the last turn in the conversation to be a user message` |
| B6 | system `""` | rejected by AWS CLI parameter validation; not sent | - | - |
| B7 | `[user "q", assistant "", user "Say hi."]` | 400 `messages: text content blocks must be non-empty` | 200 | 400 `Assistant message must have either content or tool_calls` |
| B8 | a `toolResult` with text `""` | 200 | - | - |
| B9 | `[user, assistant "Hi", assistant " there", user]` | 200 | - | - |
| B10 | two parallel tool calls, two results | 200 | - | - |
| B11 | `[user "", assistant "Hello.", user "Say hi."]` (today's placeholder) | 400, same text as B1 | - | - |
| B12 | system `" \n"` | 400 `The system field can't be blank.` | - | - |
| B13 | `[user "Name a color.", assistant "Blue.", user "  "]` | - | 200 | 200 |
| B14 | `[user "q", assistant "  ", user "Say hi."]` | - | 200 | 200 |
| B15 | `toolUse` and `toolResult` with no `toolConfig` | 400 `The toolConfig field must be defined when using toolUse and toolResult content blocks.` | - | - |

`-` means not run. The Llama 3.2 inference profile is end-of-life, so it gave no
result.

### Live tests (after the fix)

Add `#[ignore]` tests to `tests/live.rs`. Run them with
`BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5`, `AWS_REGION=eu-central-1`
and an AWS profile that can call `bedrock:InvokeModelWithResponseStream` on the
model. Record the output of a real run in the PR. A skipped run does not count.
The cases are:

- (a) user, assistant with `ToolCall`, `ToolResult`, blank user (the SMA-804
  path);
- (b) user, assistant, blank user, real user (the replay path);
- (c) user, whitespace-only assistant, `ToolCall`, `ToolResult`, assistant text,
  user (the ordering path);
- (d) assistant first, then user (the removed placeholder; Claude only);
- (e) a whitespace-only `Item::System` and a real user.

Cases (a) and (c) set `req.tools` with a `ToolDef` whose name matches the
history, because Converse needs a `toolConfig` (B15). Case (d) gates on the
model ID string (it contains `anthropic.claude`), not on `ModelFamily`, because
`global.` IDs are detected as `Unknown`.

### Unit tests in `translate/request.rs`

Compare the full `messages` array with `assert_eq!`, not only lengths. Port the
SMA-819 tests in `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`
by name, with Bedrock expected arrays. Then add the Bedrock cases below.

Drops:

- Empty text is dropped. Whitespace-only text (`"  \n\t"`) is dropped.
- Text that is not blank keeps its whitespace (`"  hi  "`).
- A user message with no blocks left is not sent.
- A user message with blank text and pending tool results keeps the
  `ToolResult` blocks.
- Blank assistant text is dropped, and an assistant message with no blocks left
  is not sent.
- A replayed sequence (real user, assistant, blank user, real user) keeps only
  the real content.
- A dropped assistant between a result and a user message keeps one user turn.
- A blank or whitespace-only system text gives no system block, and a blank
  `Item::System` inside an open round still flushes.
- `[user "", assistant a, user b]` gives `[assistant[a], user[b]]`.
- Assistant first: no placeholder. Rename
  `assistant_first_gets_synthetic_user_prepended`, and change it and
  `tool_call_item_queues_onto_assistant_turn` to match.

Ordering:

- A `ToolCall` after a pending `ToolResult` does not fold into the earlier
  assistant message.
- The same, at the end of input, and at an `Item::System` (the arrays in
  "Behavior details").
- The same, at a `UserMessage`.
- A multi-round tool loop with no assistant text alternates.
- A multi-round loop with reasoning-only assistant messages:
  `user, AM[Reasoning], t1, r1, AM[Reasoning], t2, r2, AM "done"`.
- User, whitespace-only assistant, `ToolCall`, `ToolResult`, assistant text.
- An interleaved parallel round (`t1, t2, r1, t3, r2, r3`) stays one round
  (SMA-819 `interleaved_parallel_round_is_not_split`).
- A dropped assistant inside an open round does not split the round.
- The `AssistantMessage` arm order (calls first):
  `[user q, assistant a, t1, t2, r1, t3, assistant b, r2, r3]`.

Errors:

- Case 1: only a blank user message; only `Item::System`; `Item::System` and a
  blank user message.
- Case 2: `[user "q", assistant "a", user ""]`;
  `[user q, assistant a, user "", assistant ""]`;
  `[user q, assistant a, user "", assistant[Reasoning]]`.

Not errors:

- `[user "q", assistant "a", user "", assistant "b"]`;
- `[user "q", assistant "a", ToolCall, ToolResult, user ""]`;
- `[user "q", assistant "a"]` (a caller prefill);
- `[user q, assistant a, assistant ""]` and
  `[user q, assistant a, assistant[Reasoning]]`;
- `[user q, assistant a, user []]` and `[user q, assistant a, user [Image]]`
  (no blank text, so no flag; same as `main`).

### Tests in `translate/mod.rs` and `model.rs`

- `build_request` returns the error on the `ToolChoice::None` path and on the
  normal path.
- `invoke` calls `build_request` before it makes the stream (`model.rs:49`).
  Thus `invoke(...).await.is_err()` on an offline model proves that no network
  call occurs.

## Found during the spec challenge (out of scope)

- `strip_region_prefix` (`crates/paigasus-helikon-providers-bedrock/src/family.rs:88-95`)
  strips only `us.`, `eu.`, `apac.` and `ap.`. A `global.` ID is detected as
  `ModelFamily::Unknown`. In production, `global.` profiles lose structured-output
  synthesis, forced tool choice and `parallel_tool_calls`, and the existing
  family-gated live tests skip. The mdBook
  (`docs/book/src/concepts/model-providers.md:185-188`) lists the same four
  prefixes. This needs a separate Linear issue.
- The `ToolChoice::None` path (`translate/mod.rs:129-140`) sends no
  `toolConfig`. A request with a tool history and `ToolChoice::None` thus
  probably fails with the B15 error today. This needs a separate Linear issue.

## Left to the builder

- The exact error texts, within the meaning given above.
- The name of the helper that checks for blank text.
- A direct `Err` from `items_to_messages` (both call sites in
  `translate/mod.rs` use `?`) or a flag on `TranslatedMessages`. A direct `Err`
  is the simple choice.
- Whether the translator emits a `tracing::debug!` line when it drops a block.
  If it does, the line uses `target: "paigasus::bedrock::translate"` (else
  `tests/workspace-lints/tests/tracing_target_coverage.rs` fails) and contains
  counts only, never text.
- On macOS, the Bedrock tests can fail for a reason that is not in the code
  (`NATIVE_ROOTS`, which depends on the checkout path). If they fail with that
  error, run them from a worktree under the scratchpad.

## Documentation

- The crate `README.md` and the mdBook do not describe these translation rules,
  so no edit is necessary.
- The change is a `fix(providers)` change (`providers-bedrock` is not in the
  scope allowlist). release-plz bumps the Bedrock crate and the facade. No hand
  version bump.

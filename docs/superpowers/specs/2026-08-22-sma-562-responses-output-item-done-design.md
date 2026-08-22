# SMA-562 — `ResponsesTranslator` reconciles tool calls it never emitted

**Date:** 2026-08-22
**Ticket:** [SMA-562](https://linear.app/smaschek/issue/SMA-562/openairesponses-reports-finishtoolcalls-for-a-function-call-that)
**Related:** SMA-533 (cross-provider stream conformance suite — where the defect was found),
SMA-522 (the `Usage`/`Finish` ordering invariant this must preserve)
**Classification:** bounded — two `match` arms and one helper in one file, its tests, one
captured fixture, one live-gated assertion.

## Acceptance criteria (verbatim from SMA-562)

> * Determine whether a zero-argument tool call streams any
>   `function_call_arguments.delta` frames, by capture rather than by reading docs.
> * If reachable, emit the tool call from `output_item.done` so the `ToolCallDelta` and the
>   `Finish{ToolCalls}` agree.
> * A test drives the zero-delta sequence and asserts the two agree, verified to fail
>   against the current translator.

| AC | Where it is discharged |
|----|------------------------|
| 1 — capture, don't read docs | §2.1 (falsified on two model families), §2.2 (a delta-free stream found by capture) |
| 2 — emit from `output_item.done` | §4 step A. **Deviation:** `output_item.done` alone does not make the two agree — §3 is the proof, §4 step B is the addition that does. Flagged for approval at GATE 1. |
| 3 — a failing test | §6 tests 1, 2 and 5; each names the assertion that fails today and why |

## 1. The reported defect

`ResponsesTranslator` (`crates/paigasus-helikon-providers-openai/src/backend/responses.rs`)
derives two things from two different places:

- a `ToolCallDelta` is emitted **only** from `response.function_call_arguments.delta`
  (`:364-377`) or from the `pending_args` flush inside the `output_item.added` arm
  (`:342-352`);
- `Finish { reason: ToolCalls }` comes from `!item_to_call.is_empty()` (`:401`, `:503`), and
  `item_to_call` is populated **only** from `response.output_item.added` (`:337-341`).

Nothing ties the two together, so the stream can lie in either direction.

## 2. What the capture found

AC 1 asks whether this is reachable, **by capture rather than by reading docs**. Both
captures below were taken against `https://api.openai.com/v1/responses` on 2026-08-22 with
`curl`, so the bytes are the wire's, not async-openai's rendering of them.

### 2.1 The hypothesised trigger is falsified

A zero-argument strict tool (`{"type":"object","properties":{},"required":[],
"additionalProperties":false}`, `tool_choice: "required"`) streams **one** argument delta,
carrying `"{}"`. On `gpt-4o-mini-2024-07-18`:

```
response.output_item.added              arguments:"" call_id:call_8xWY… name:get_current_time
response.function_call_arguments.delta  delta:"{}"
response.function_call_arguments.done   arguments:"{}"
response.output_item.done               arguments:"{}"
response.completed                      status:"completed"
```

`gpt-5-mini` produces the same shape (preceded by a `reasoning` item, which the
`output_item.added` arm already ignores because it is not an `OutputItem::FunctionCall`).

So **a zero-argument tool is not the trigger**, and the translator handles zero-argument
tools correctly today. The ticket named this "the obvious candidate"; it is not one.

### 2.2 A delta-free tool-call stream is real — and fails the other way

Resuming a stored background response past its argument deltas produces a stream that
describes a tool call entirely on `output_item.done`. Sent `background: true, store: true`
(which by itself streams perfectly ordinarily — see §2.4), observed the deltas at sequence
4–8, then issued a **second, separate** request,
`GET /v1/responses/{id}?stream=true&starting_after=9`:

```
response.output_item.done   item:{type:"function_call", call_id:"call_lQOsuE9…",
                                  name:"get_weather", arguments:"{\"city\":\"Berlin\"}"}
response.completed          status:"completed"
```

No `output_item.added`. No argument deltas. Against the current translator: `item_to_call`
stays empty, so `has_tool_calls` is `false`, and the turn ends `Finish { Stop }` having
**silently dropped a fully-described tool call**.

### 2.3 What the terminal event always carries

Every one of the four captures carries the complete function-call items in
`response.completed`'s `output` array — including the resumed one:

```json
"output":[{"call_id":"call_lQOsuE9Lx2s6d70xJ88uClEk","id":"fc_0aae0e68…",
           "status":"completed","type":"function_call","name":"get_weather",
           "arguments":"{\"city\":\"Berlin\"}"}]
```

`Response::output` is `Vec<OutputItem>` — not `Option` — and is already deserialized on
every `ResponseCompleted` event the translator handles today (`:397-402`). §4 uses it.

### 2.4 Honest statement of reachability

`build_request` never sets `background`, and the crate exposes no retrieve-or-resume entry
point (`grep` over `providers-openai/src` finds only `previous_response_id`), so §2.2's
stream **cannot be produced through `OpenAiModel::responses(...)` today**.

It is also **not "one flag away"**, and an earlier draft of this spec said so wrongly. The
`background: true` request in the capture streamed `output_item.added` and five argument
deltas exactly like a foreground call. The delta-free stream required a *second* request to a
resume endpoint that this crate has no code path to. Reaching it needs `background`/`store`
**plus** a retrieve-and-resume surface that does not exist — explicitly out of scope here
(new public surface, a new `ModelSettings` field, book and README updates, none of which
this ticket asks for).

The fix still belongs in the translator, for a reason that does not depend on §2.2 being
reachable: §3 shows the translator's correctness currently rests on two undocumented API
behaviours that nothing in this repo asserts, tests, or controls, and §2.1 is exactly the
shape that makes one of them tempting for OpenAI to drop — a delta carrying `"{}"` conveys
nothing.

## 3. Why `output_item.done` alone is not the fix

AC 2 says to emit from `output_item.done`. That repairs §2.2, but it does **not** make
`ToolCallDelta` and `Finish{ToolCalls}` agree, and an earlier draft of this spec claimed it
did. The counter-example is the shape §1 literally reports:

> `output_item.added` → (no deltas) → `response.completed`, with no `output_item.done`.

`item_to_call` is non-empty at `:401`, so `terminal_events` still returns `ToolCalls` at
`:503` with zero `ToolCallDelta` emitted. A `done`-only fix therefore swaps the unverified
invariant *"a function call always streams at least one argument delta"* for the equally
unverified *"a function call always emits an `output_item.done`"*. Neither is asserted
anywhere.

The same gap swallows a third shape: deltas buffered into `pending_args` when neither
`output_item.added` nor `output_item.done` ever arrives are dropped in silence —
`pending_args` is drained at exactly one site, `:342`.

**The terminal event is the only point where the question is decidable**, because §2.3 shows
it always carries the full item list. Reconciling there is what makes the post-condition an
invariant rather than a hope.

### 3.1 Post-condition (this is what the tests assert)

> For a stream terminating in `response.completed`, the translator emits
> `Finish { ToolCalls }` **if and only if** it emitted at least one `ToolCallDelta`; and
> every `function_call` item in the terminal `response.output` has had exactly one
> `ToolCallDelta` carrying `name: Some(_)`.

Deliberately scoped to `response.completed`. §4.3 says why `response.incomplete` is excluded.

**Delivered by a gate, not assumed.** `response.completed`'s `has_tool_calls` reads
`!name_emitted.is_empty()` evaluated *after* step B's reconciliation sweep (§4), so the "iff"
holds by construction — see §4.5 for why this differs from gating on `name_emitted` *instead
of* reconciling (rejected) versus gating on it *after* reconciling (shipped). Two carve-outs
are deliberate, not gaps in the guarantee: a `function_call` item with `id: None` is never
emitted (§4.2 — no correlator to dedup on), and one whose `status` is anything other than
absent or `Completed` — i.e. `Incomplete` **or** `InProgress` — is never emitted (§4.3 —
its `arguments` may be truncated JSON). Both are absent from "every `function_call` item
... has had exactly one `ToolCallDelta`" by carve-out, not by defect.

## 4. The change

One private helper plus two arms in `ResponsesTranslator::consume`. The helper is the single
place the emission rule lives:

```
fn emit_call_if_unseen(&mut self, item: &OutputItem) -> Option<ModelEvent>
```

Given an `OutputItem::FunctionCall(fc)` with `id: Some(item_id)`:

1. **Skip unless complete** — return `None` when `fc.status` is anything other than `None`
   or `Some(OutputStatus::Completed)` — i.e. a whitelist, not a blacklist of `Incomplete`
   alone, so it also skips `Some(OutputStatus::InProgress)` — *before* registering into
   `item_to_call` (§4.3). `OutputStatus` has exactly three variants and is not
   `#[non_exhaustive]`, so a whitelist is total where a blacklist would silently admit any
   future variant. Order matters: registering first and checking status second would let a
   non-complete item land in `item_to_call` while emitting nothing — the same shape that
   makes a naive `has_tool_calls` check report `ToolCalls` with zero `ToolCallDelta`s emitted
   (§3.1's post-condition would fail).
2. **Register** — `item_to_call.entry(item_id).or_insert((call_id, name))`, so
   `has_tool_calls` is correct even when `output_item.added` never arrived (§2.2). Because
   step 1 already returned for a non-complete item, the helper only ever registers an item it
   still intends to emit (modulo the "already emitted" skip below, which is an idempotent
   re-registration, not a false positive).
3. **Skip if already emitted** — return `None` when `name_emitted` contains `item_id`.
4. **Otherwise emit** — discard any `pending_args` for `item_id` **unless** `fc.arguments`
   is empty and the buffer is not (in which case the buffer is the better data), insert
   `item_id` into `name_emitted`, and return
   `ToolCallDelta { call_id, name: Some(name), args_delta }`.

Anything else — a non-`FunctionCall` item, or a `FunctionCall` with `id: None` — returns
`None`. **Correction, post-implementation:** only the `id: None` case logs a `tracing::debug!`
(preserving the log the trailing `other` arm gives those events today, §4.4); a non-
`FunctionCall` item returns `None` silently, with no log. This is deliberate, not an
oversight: the `response.completed` sweep calls this helper over *every* item in
`response.output`, including every `message` and `reasoning` item on every response — logging
there would debug-log once per non-tool output item on every single completed response, which
is not the sparse, exceptional signal `tracing::debug!` is for elsewhere in this file. The
`id: None` case stays logged because it is actually exceptional: a `FunctionCall` item is
expected to always carry an `id`, so seeing one without is a signal worth surfacing.

**Step A — `ResponseStreamEvent::ResponseOutputItemDone`** (this is AC 2). Calls the helper
and yields its `Option` as a 0-or-1-element `Vec`. This is the earliest point a call is
fully known, so in a delta-free stream the consumer sees it before the terminal event rather
than at it.

**Step B — the `ResponseCompleted` arm** (this is what closes §3). Reconcile *before*
building the terminal pair:

```rust
let mut out: Vec<ModelEvent> = e.response.output.iter()
    .filter_map(|item| self.emit_call_if_unseen(item))
    .collect();
out.extend(terminal_events(
    e.response.usage, e.response.status, None, !self.name_emitted.is_empty(),
));
Ok(out)
```

**Shipped, revising the above:** `has_tool_calls` is `!self.name_emitted.is_empty()`, not
`!self.item_to_call.is_empty()`, evaluated *after* the sweep. §3.1 and §4.5 explain why.

Both arms share the helper and the same dedup key, so they compose idempotently: in the
normal path (§2.1) the deltas emit, `done` sees `name_emitted` and returns `None`, and
`completed` does too. Steps A and B emit **no `Usage`**; `terminal_events` remains the sole
constructor of `Usage` and still appends `Finish` last, so SMA-522's invariant (`:473-477`)
holds unchanged.

### 4.1 Why `name_emitted` is the right dedup key

`name_emitted` already means precisely "a `ToolCallDelta` has been emitted for this
`item_id`": it is inserted at exactly two sites — `:347` (the `pending_args` flush, correctly
guarded on `!buffered.is_empty()`, so an empty buffer is not marked emitted) and `:370` (the
delta arm) — and nowhere else. Reusing it needs no new state and cannot drift from the
emission sites.

### 4.2 Why `id: None` is skipped

Dedup needs the `item_id` correlator; without it we cannot tell a fresh call from one whose
deltas already streamed, and emitting would risk duplicating a whole arguments string. Both
captures carry `id`, and the `output_item.added` arm already requires it.

### 4.3 Why `response.incomplete` does not reconcile

A turn truncated by `max_output_tokens` can carry an `output_item` with
`status: "incomplete"` and a **partial, unparseable** `arguments` string. Today no deltas
means no `ToolCallDelta`, and `ModelTurnAccumulator::finish` succeeds. Reconciling there
would emit unparseable args and make `build_items` return
`Err("invalid tool args for call_id=…")` (`crates/paigasus-helikon-core/src/model.rs:530-535`),
failing the **entire turn** — a strictly worse unhappy path than the one being fixed.

So: the `ResponseIncomplete` arm is untouched, and step 1 of the helper also skips any
non-complete status — `OutputStatus::Incomplete` **and** `OutputStatus::InProgress` — on the
`completed` path, not `Incomplete` alone: a whitelist of `None | Some(Completed)`, rather
than a blacklist of `Incomplete`. `OutputStatus` has exactly three variants and is not
`#[non_exhaustive]`, so the whitelist is total — it cannot silently admit some future fourth
variant the way a blacklist would. For `response.incomplete` this costs nothing at the
finish reason, because `terminal_events` already ignores
`has_tool_calls` whenever `incomplete_reason` is `Some` (`:495-501`, guarded by an existing
test at `:690`). **That reasoning does not transfer to `response.completed`**: there,
`incomplete_reason` is `None`, so `has_tool_calls` does steer the finish reason. That is
exactly why `response.completed`'s `has_tool_calls` reads `name_emitted` — set only when the
helper actually emits — rather than `item_to_call`, which `output_item.added` still populates
unconditionally, non-complete items (`Incomplete` or `InProgress`) included; reading
`item_to_call` there would let a never-emitted non-complete call flip the finish reason to
`ToolCalls`.

### 4.4 Assumptions stated rather than left implicit

- **`output_item.done` is terminal for its item.** A delta arriving *after* its `done` would
  find `name_emitted` set and append to an already-complete args string, producing malformed
  JSON — e.g. `{"city":"Berlin"}{"city":"Berlin"}` — which fails `build_items` for the WHOLE
  turn, not just this one call. Not observed (a resumed stream truncates a prefix, so order is
  preserved). Recorded in §4's arm comment because the translator elsewhere goes out of its
  way to tolerate reordering.
- **`output_index` is ignored deliberately.** `item.id` is the unique per-item correlator;
  `output_index` adds nothing the maps do not already key on.
- **Arguments pass through verbatim.** `build_items` normalizes blank args to `{}`
  (`core/src/model.rs:525-529`), so an empty `args_delta` is safe for
  `ModelTurnAccumulator` consumers. It is *not* normalized by the AG-UI mapper
  (`runtime-agentcore/src/agui/map.rs:158-190`), which forwards `args_delta` into
  `TOOL_CALL_ARGS` verbatim — an AG-UI client would see an empty args span rather than
  `{}`. Accepted: synthesising `{}` where the wire said `""` would misreport the wire, and
  step 4's buffer-preference rule already covers the case where better data exists.
- **Orphaned `pending_args`** are logged with `tracing::warn!` in the `response.completed`
  arm when non-empty. Only that arm warns: `response.incomplete` is deliberately untouched
  (§4.3), so it neither reconciles nor logs a buffer left over for an item that turns out
  truncated. They remain dropped either way — reconciliation from `response.output`
  supersedes them where it runs — but silently dropping data is what §1 is about, so it
  becomes observable at least on the `completed` path.

### 4.5 Rejected alternatives

- **`output_item.done` only** (AC 2 as literally written) — §3. Repairs §2.2, leaves §1.
- **Gate `has_tool_calls` on `!name_emitted.is_empty()` instead of reconciling.** Rejected in
  that form: skipping step B's `response.output` sweep and gating on `name_emitted` alone
  makes the two agree in the fail-safe direction, but by *dropping* a call the API fully
  described — that is §2.2's bug, not a fix for it. **What shipped is different, and is not
  this rejected alternative:** gate on `!name_emitted.is_empty()` *after* step B's sweep has
  already run. By that point every emittable call has already been emitted (and is therefore
  in `name_emitted`), so reading it there drops nothing — it is a refinement of the
  reconciling design, not a substitute for it. (`!item_to_call.is_empty()` is kept for the
  untouched `response.incomplete` arm — §4.3.)
- **`response.function_call_arguments.done` as the reconciliation point.** It carries
  `item_id` and complete `arguments`, but no `call_id` and no `name`, so a call that never
  saw `output_item.added` could not be emitted from it. `output_item.done` and
  `response.output` carry all three.
- **Reconcile argument *content* on `done`** (diff `done.arguments` against what the deltas
  accumulated). Needs per-item accumulated-args state nothing else wants, and the only shape
  it repairs — deltas that streamed a strict prefix — was not observed. YAGNI.
- **Close as not-reproducible.** Honest to the ticket's literal wording, but §2.2 is a
  captured dropped-call bug and §3 is a reachable-today one.

## 5. Not in scope

- **`background` / resume request surface** — §2.4.
- **The cross-provider conformance suite** gets no new scenario. Its `Scenario` enum is
  shared by five providers and each must model every variant; a resumed-stream wire shape is
  specific to the Responses API with no analogue in Chat Completions, Anthropic, Gemini or
  Bedrock. (Its `output_item_done` *comment* is corrected — §7.)
- **Non-function tool items.** `OutputItem` also has `CustomToolCall`, `McpCall`,
  `LocalShellCall`, `ShellCall` and `ApplyPatchCall` variants. `build_request` builds only
  `Tool::Function` (`:111-125`), so none is reachable through this crate, and all are
  ignored identically by the existing and new arms. Worth a follow-up ticket if hosted-MCP
  tools are ever passed through; not this one.

## 6. Tests

Tests 1, 2 and 5 must be **run against unmodified `main` first** and their observed failure
output recorded in the commit message, per AC 3. Tests 1–4 are unit tests in the
`#[cfg(test)] mod tests` of `backend/responses.rs`, which constructs typed
`ResponseStreamEvent` values directly (`:526-550`) and parses no SSE.

1. **`done_without_added_or_deltas_emits_tool_call`** (shipped as this name; corrected
   post-implementation from this draft's `resumed_stream_emits_tool_call_from_done`) — §2.2's
   sequence: `output_item.done` +
   `response.completed`, no `added`, no deltas. Asserts one `ToolCallDelta`
   (`call_id == "call_lQOsuE9Lx2s6d70xJ88uClEk"`, `name == Some("get_weather")`,
   `args_delta == "{\"city\":\"Berlin\"}"`) and a terminal `Finish { ToolCalls }`.
   *Fails today:* no `ToolCallDelta` is emitted, and the finish reason is `Stop` because
   `item_to_call` is empty at `:401`.
2. **`added_then_completed_without_deltas_emits_tool_call`** — §3's counter-example, and the
   shape SMA-562 literally reports: `added` → `completed`, **no `done`**, no deltas.
   *Fails today:* `Finish { ToolCalls }` with zero `ToolCallDelta`.
   This is the test a `done`-only fix would still fail, which is why it exists. The sequence
   is **synthetic** — §2.1 could not capture it — and its doc comment says so, per this
   crate's fixture-provenance discipline.
3. **`done_after_deltas_does_not_double_emit`** (shipped name; corrected post-implementation
   from this draft's `done_then_completed_does_not_double_emit` — and split in two: the
   `done`-half above is this test, while the `completed`-half — `added` → delta → `completed`
   yields only `[Usage, Finish]` with no repeated arguments — actually lives in the separate
   `completed_after_deltas_emits_only_terminal_pair` test, not inside this one) — `added` →
   two deltas → `done`. Asserts `done` yields zero events. Passes today; must keep passing.
4. **`parallel_calls_emit_one_named_delta_each`** — two interleaved items:
   `added(A)`, `added(B)`, deltas for B only, `done(A)`, `done(B)`, `completed`. Asserts
   exactly one `name: Some(_)` delta per `call_id` and correct args attribution. The design
   is per-item correct (every map keys on `item.id`, `:250-262`) but nothing asserts it, and
   the conformance suite enforces the same rule cross-provider
   (`tests/provider-stream-conformance/src/check.rs:220-245`) where a regression would
   surface with no unit-level signal.
5. **`incomplete_status_item_is_not_emitted`** — a `completed` event whose `output` carries a
   `status: "incomplete"` function call with truncated `arguments`. Asserts no
   `ToolCallDelta` (§4.3).
   *Fails today* in the sense that matters: it fails against the **naive** version of this
   fix (one that omits step 1), which is the regression it exists to prevent. Against
   unmodified `main` it passes vacuously; the commit message says so rather than claiming a
   red-to-green transition it does not have.
6. **`zero_argument_tool_streams_one_delta`** — an integration test in
   `tests/responses_streaming.rs` over the §2.1 fixture: one `ToolCallDelta` with
   `args_delta == "{}"`, and `Finish { ToolCalls }`. A **regression pin**, nothing more — a
   frozen fixture cannot notice an upstream behaviour change.
7. **`responses_zero_arg_tool_streams_a_delta`** — in `tests/live.rs`, `#[ignore]` and
   `OPENAI_API_KEY`-gated like its siblings (`:47-61`). Drives a real zero-argument tool and
   asserts exactly one `ToolCallDelta` (`name == Some("get_current_time")`,
   `args_delta == "{}"`) followed by a terminal `Finish { ToolCalls }`.
   **Correction, post-implementation:** this was originally described as "the only place
   §2.1's finding can be re-verified." It no longer is. For this real captured stream shape
   (which does carry a `response.output_item.done`), it is the `output_item.done` arm — and,
   for a stream lacking one, the `response.completed` sweep — that makes a live stream with
   the `{}` delta and one without it produce byte-identical `ModelEvent` sequences — the
   `output_item.done` arm synthesises the missing delta from the terminal item — so §2.1's
   elision question is no longer observable through the public `ModelEvent` stream, by this
   test or by test 6's fixture. That is the fix working as intended, not a gap in this test.
   Its value is now an end-to-end pin against the live API (name, args, finish reason), not
   a change-detector for the delta specifically.

### 6.1 Fixtures

**One** new file: `crates/paigasus-helikon-providers-openai/tests/fixtures/
responses_tool_call_zero_args.txt` — §2.1, CAPTURED, headed with the provenance block this
directory already uses (`responses_tool_call.txt` is the model), stating endpoint, model,
date, request shape and every deliberate edit. It is covered by the existing
`.gitattributes` rule `crates/paigasus-helikon-providers-openai/tests/fixtures/*.txt text
eol=lf` (`.gitattributes:15`, SMA-522), so no `.gitattributes` change is needed.

**No fixture for §2.2.** The integration harness `run()` (`tests/responses_streaming.rs:29-50`)
mocks only `POST /responses`; the resumed stream is a `GET .../{id}?stream=true&starting_after=9`.
Serving it from the POST mock would be a fiction, so §2.2 drives test 1 as typed events and
its bytes are quoted in §2.2 and in the test's doc comment instead.

### 6.2 The `[DONE]` sentinel — settled empirically

Every existing fixture in that directory ends `data: [DONE]`
(`responses_tool_call.txt:104`). **None of the four raw captures contains it** — the live
endpoint did not send one.

Resolved by probe on 2026-08-22, before this plan was written: the §2.1 capture was served
through `run()` both byte-faithful and with `data: [DONE]\n\n` appended. **Both terminate
cleanly with identical output** — zero errors, and in both cases exactly
`[ToolCallDelta { call_id: "call_8xWY…", name: Some("get_current_time"), args_delta: "{}" },
Usage { .. }, Finish { ToolCalls }]`. async-openai's SSE stream ends on body end; the
sentinel is not required.

**Decision: ship the fixture byte-faithful, with no `[DONE]`**, and say in the provenance
header that the sentinel is absent because the endpoint did not send one. Nothing is
fabricated, and the CAPTURED label stays honest.

That same probe independently confirms §2.1 end to end: the current translator already
handles a zero-argument tool correctly, which is what makes it a regression pin (test 6)
rather than a fix.

The header also notes that `responses_tool_call.txt`'s own `[DONE]` is now unexplained —
it claims faithful transcription of the same endpoint. Auditing that older fixture's
provenance is a follow-up ticket, not this PR.

## 7. Documentation

Six comment sites in this crate become stale (an earlier draft named only one, and named it
wrongly — `terminal_events`' own doc at `:453-477` says merely "the caller passes
`!item_to_call.is_empty()`", which this draft claimed stays true).

**Correction, post-implementation:** it does not stay true. §4's shipped `ResponseCompleted`
arm passes `!name_emitted.is_empty()`, not `!item_to_call.is_empty()`, so that doc's claim
is false for that caller — it became a seventh stale site, fixed alongside the other six
(it now says which caller passes which expression):

1. `:215-221` — the `ResponsesTranslator` event-list bullets: add `output_item.done`, and
   restate what `response.completed` now does.
2. `:247-250` — `name_emitted`: now also set by the two reconciliation sites.
3. `:251-255` — `item_to_call`: no longer "Populated by `response.output_item.added`" alone.
4. `:257-262` — `pending_args`: no longer only "flushed … the moment `output_item.added`
   registers"; §4 step 4 may discard it, and §4.4 warns on orphans.
5. `:397-402` — the `ResponseCompleted` arm gains the reconciliation rationale and a pointer
   to §3.1's post-condition.
6. `tests/provider-stream-conformance/tests/conformance.rs:2968-2971` — documents
   `output_item.done` as carrying "the same no-op-to-the-translator caveat". That becomes
   false for the OpenAI Responses subject and must be corrected even though §5 adds no
   scenario.

**mdBook and READMEs: no change.** Deliberate call, not a silent skip. No public API,
feature flag, crate roster or quickstart flow changes;
`docs/book/src/concepts/model-providers.md` describes the `ModelEvent` vocabulary, not
per-provider wire shapes.

## 8. Release and rollback

`paigasus-helikon-providers-openai` is published (`Cargo.toml:4` — `0.2.24`, no
`publish = false`, no `release = false` block in `release-plz.toml`) and consumes no new
`core` API, so it rides release-plz's normal flow: no stub-ascend ritual, no manual `core`
or facade bump.

**Rollback:** revert the commit. The change is additive — two arms and a helper, no
signature or behaviour change to any existing arm — so nothing depends on it having landed.
The risk it would be reverted for is double-emission against a wire shape not captured here;
test 3 and test 4 are the guards, and §4's shared dedup key means any such bug would be in
one helper rather than spread across arms.

## 9. Raw capture disposition

The four raw captures live **outside the git worktree**
(`scratchpad/captures-sma-562/`) and are not committed. The probe script sources the repo's
`.env`, and `.gitignore` (lines 1-13) does not cover a scratch directory — given this repo's
standing "never `git add -A`" hazard, keeping them out of the tree entirely is the safe
disposition. Their content survives in this spec (§2.1, §2.2, §2.3), in the §2.1 fixture, and
in test 1's doc comment.

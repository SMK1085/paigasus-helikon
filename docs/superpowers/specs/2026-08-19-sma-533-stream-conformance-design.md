# SMA-533 — Cross-provider conformance suite for stream event-ordering

**Status:** approved (brainstorm), pending adversarial challenge
**Date:** 2026-08-19
**Branch:** `feature/sma-533-add-a-cross-provider-conformance-suite-for-stream-event`
**Ticket:** [SMA-533](https://linear.app/smaschek/issue/SMA-533/add-a-cross-provider-conformance-suite-for-stream-event-ordering)

## 1. Problem

Six streaming translators independently re-derive the event-ordering contract on
`paigasus_helikon_core::Model::invoke`, and nothing asserts it. Two have already
got it wrong, in different ways:

- **OpenAI** emitted `Finish` before `Usage` on every streaming turn (fixed in
  PR #197). It went undetected because the fixtures encoded a wire shape that
  does not occur.
- **Anthropic** emitted no `Finish` at all when a stream truncated between
  `message_delta` and `message_stop` (fixed in PR #200).

The six translators are `openai/backend/chat.rs`, `openai/backend/responses.rs`,
`anthropic/stream.rs`, `bedrock/stream.rs`, `gemini/stream.rs`, and
`litellm/stream.rs` — five crates, because OpenAI carries two backends.

## 2. Ticket corrections

Three statements in the ticket are stale or incomplete as of `origin/main`
`0bf5e759`. They are corrected here so the implementation is not built on them.

**2.1 — The SMA-550 caveat no longer applies.** The ticket's *Ordering vs
SMA-550* section says `providers-litellm` fails assertion 7 today and that
SMA-533 must either land SMA-550 first or scope the assertion knowingly.
SMA-550 merged as `0bf5e759` (PR #208). Assertion 7 is asserted
unconditionally for all six subjects, with no documented exception.

**2.2 — The `finish()` return shape diverges four ways, not three.** The ticket
lists three. `openai/backend/responses.rs` has **no `finish()` at all** — its
terminal event builds `Usage` and `Finish` together from one upstream event, so
there is nothing to flush. `consume` diverges too: Anthropic and Responses
return an outer `Result<_, ModelError>`; the other four do not.

| Subject | `consume` returns | `finish()` returns |
| --- | --- | --- |
| `openai/backend/chat` | `Vec<ModelEvent>` | `Vec<ModelEvent>` |
| `openai/backend/responses` | `Result<Vec<ModelEvent>, ModelError>` | *(absent)* |
| `anthropic` | `Result<Vec<Result<ModelEvent, ModelError>>, ModelError>` | `Option<Result<ModelEvent, ModelError>>` |
| `bedrock` | `Vec<Result<ModelEvent, ModelError>>` | `Option<Result<ModelEvent, ModelError>>` |
| `gemini` | `Vec<Result<ModelEvent, ModelError>>` | `Vec<Result<ModelEvent, ModelError>>` |
| `litellm` | `Vec<ModelEvent>` | `Vec<ModelEvent>` |

**Decision: settling this shape is deferred to a follow-up ticket.** The suite
asserts at the `Model::invoke` boundary and never observes `finish()`, so
unifying it serves no assertion in this PR, while a five-crate signature
refactor carries a different risk profile from adding tests. The suite lands
first and then guards the refactor when it happens. A follow-up Linear issue is
filed as part of this work.

**2.3 — No manual version bumps.** The ticket correctly notes the doc-only
`-core` edit carries a patch bump plus the facade cascade, but that happens
*automatically*. CLAUDE.md's manual-bump ritual applies only when an ascending
stub needs same-PR `-core` API to survive `cargo publish --verify`. Nothing
ascends here and the new crate is `publish = false` from birth. Because `-core`
is already released, release-plz performs the bump itself — which is the
precise condition under which `dependencies_update` cascades to the facade.
Hand-bumping would *defeat* that cascade and strand the facade, the
second-order caveat CLAUDE.md documents. **Do not edit any `version` field.**

## 3. Decisions taken

| # | Decision | Rationale |
| --- | --- | --- |
| D1 | Assert at the `Model::invoke` boundary, against a mock transport | The contract is *stated* on `invoke`. A translator-only harness cannot catch SMA-531, the defect that motivated this ticket — see §4. |
| D2 | Suite lives in `tests/provider-stream-conformance` | Follows `tests/runtime-http-conformance`, the existing cross-crate conformance suite; removes the dev-dep cycle rather than working around it — see §5. |
| D3 | `finish()` shape deferred to a follow-up ticket | §2.2 |
| D4 | Green on merge; escape hatch needs sign-off | A suite that lands red teaches people to ignore it — which is how the OpenAI bug shipped past green fixtures. See §9. |
| D5 | Fold in SMA-532 | Two doc comments in `bedrock/src/stream.rs` assert the exact misreading this PR's core wording exists to kill, in a file this suite now tests. |

## 4. Why the Model boundary, not the translator

Every one of the six drivers is an inline `stream!` block with the same shape:

```rust
loop {
    let next = tokio::select! {
        biased;
        _ = cancel.cancelled() => return,   // no Finish
        n = upstream.next()    => n,
    };
    match next {
        None          => { flush translator.finish(); return }
        Some(Err(e))  => { yield Err(map(e)); return }        // no Finish
        Some(Ok(ev))  => { for e in translator.consume(ev) { yield e } }
    }
}
```

Assertions 5 (cancellation) and 6 (mid-stream error) are properties of **that
loop**. The translator never observes either — it is not called on those paths.

More decisively: **SMA-531 was a driver defect, not a translator defect.**
Anthropic's translator correctly buffered the stop reason; `model.rs:113` was a
bare `None => return` with no flush. A harness that supplies its own loop and
calls `finish()` itself would have gone green against that exact defect. That is
the "conformance suite that cannot fail" failure mode the acceptance criteria
name.

The cost of the Model boundary is per-provider wire fixtures. That cost is
accepted.

## 5. Architecture

New workspace member `tests/provider-stream-conformance`, mirroring
`tests/runtime-http-conformance`:

```
tests/provider-stream-conformance/
├── Cargo.toml           version = "0.0.0", publish = false
├── src/lib.rs           Scenario, checker, PacedServer, non-conformant fakes
│                        deps: paigasus-helikon-core, async-trait, futures-util, tokio
├── tests/conformance.rs registers the six subjects, runs the table
│                        dev-deps: all five provider crates
└── fixtures/            per-subject wire scripts
```

`release-plz.toml` gets a `publish = false` / `release = false` block, matching
the two existing internal crates.

**The dependency arrow points one way: suite → providers.** No provider crate
depends on the suite, so the dev-dependency cycle that deadlocked release-plz in
SMA-326 cannot arise, and no path-only version-less trick is needed. This is the
main reason to prefer the `runtime-http-conformance` precedent over the
`sessions-testkit` one the ticket names — `sessions-testkit` is dev-depended by
its backends, which is the shape that carries the cycle hazard.

Placing it under `tests/` rather than `crates/` also leaves the crate roster,
the facade, and every README untouched. `docs/book/src/getting-started/
workspace-layout.md`'s "the workspace's 21 without publishing" counts `crates/`
only — `runtime-http-conformance` already sits outside that count — so that
sentence stays true and needs no edit.

### 5.1 The seam

`src/lib.rs` knows nothing about any provider:

```rust
pub enum Scenario { /* §6 */ }

/// What a subject did with a scenario. Declining is a first-class outcome with
/// a mandatory reason, not an `Option` a caller can silently treat as a skip.
pub enum Outcome {
    Served(BoxStream<'static, Result<ModelEvent, ModelError>>),
    /// The wire shape cannot physically occur for this provider. The reason is
    /// printed in the report.
    Declined(&'static str),
}

#[async_trait]
pub trait StreamUnderTest {
    /// Human-readable subject name, e.g. "openai/chat". Used in failure output.
    fn name(&self) -> &'static str;

    /// Serve `scenario`'s bytes and return this provider's `Model::invoke` stream.
    async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome;
}

pub async fn assert_conforms(subject: &impl StreamUnderTest);
```

Making `Declined` a variant rather than a `None` means the reason cannot be
omitted and the report cannot lose the distinction between "did not apply" and
"was not run".

Each subject's registration in `tests/conformance.rs` is roughly twenty lines:
start the `PacedServer` with that scenario's chunks, build the model against it,
call `invoke`.

### 5.2 The paced server

wiremock writes the whole body in a single `set_body_raw`, so there is no pacing
to interrupt. `litellm/tests/cancellation.rs` already documents this as the
reason a true mid-stream cancel goes untested, and the OpenAI provider's
streaming tests carry the same note.

That limitation makes assertion 5 untestable in any meaningful sense.
Cancel-*before*-response is satisfied trivially by any implementation; cancel
*after a stop reason has been buffered* is the case that separates a correct
driver from one whose EOF path fires `finish()` anyway.

So the suite ships a `PacedServer`: a `tokio::net::TcpListener` that writes a
canned HTTP/1.1 response head plus a `Transfer-Encoding: chunked` body, emitting
a `Vec<Vec<u8>>` of chunks with a release gate between them.

Being byte-oriented rather than SSE-oriented, one server serves all five crates:
`text/event-stream` for OpenAI, Anthropic, Gemini and LiteLLM, and
`application/vnd.amazon.eventstream` frames for Bedrock. That collapses what
would otherwise be a wiremock-versus-`StaticReplayClient` split into one
mechanism. `aws-smithy-eventstream 0.61.2` is already in the graph for building
Bedrock's frames.

Reachability per provider: `base_url()` on OpenAI, Anthropic, Gemini and
LiteLLM; `SdkConfig::endpoint_url` plus static test credentials on Bedrock.

## 6. Scenarios

Seven scenarios, each a chunk list for the `PacedServer`.

| # | Scenario | Wire script | Asserts |
| --- | --- | --- | --- |
| S1 | `CleanStop` | deltas → stop-reason chunk → **usage chunk** → terminator → clean EOF | 1, 2, 3 |
| S2 | `TruncatedAfterStopReason` | deltas → stop reason observed → body ends cleanly, no terminal frame | 3 |
| S3 | `TruncatedMidGeneration` | deltas → body ends cleanly, no stop reason ever observed | 4 |
| S4 | `MidStreamError` | deltas → connection dropped without the terminating chunk | 6, 1 |
| S5 | `CancelAfterStopReason` | deltas → stop reason observed → **gate**; harness fires the token | 5 |
| S6 | `FragmentedToolName` | tool-call deltas, name split across ≥2 fragments, id resolving after the first | 7, 1 |
| S7 | `ToolCallCleanStop` | one complete tool call → tool-use stop reason → terminator | 1, 3, 7 |

**S1 must place usage *after* the stop reason.** That is the real OpenAI wire
order. SMA-522 went undetected precisely because the fixtures encoded a shape
that does not occur, so every fixture here is transcribed from captured or
already-committed traffic — the repo has fixture directories for LiteLLM
(captured against 1.97/1.98), Anthropic and OpenAI — and never invented from
vendor documentation.

**S3 and S4 differ only in how the server closes**, which is why the raw-TCP
server earns its place. S3 writes the terminating `0\r\n\r\n` and closes: the
client sees a clean EOF mid-generation, no stop reason was observed, and no
`Finish` may be emitted. S4 drops the connection *without* it: the client sees a
truncated body, yields `Err`, and still emits no `Finish`. wiremock cannot
express that distinction at all.

**S6 is not expressible for four of the six subjects.** Anthropic, Bedrock,
Gemini and OpenAI Responses all deliver the tool name whole, in a single
`content_block_start` / `toolUse` / `functionCall` / `output_item.added` event —
there is no fragment to split. Those four decline S6 with a reason. Assertion 7
is still checked for all six via S7, whose "at most one name-carrying delta per
`call_id`" holds regardless of fragmentation.

A scenario a provider cannot physically produce is not the same thing as an
assertion it fails, and the report must not let the two look alike.

## 7. Assertions

1. **`Finish`-terminated.** Nothing follows `Finish`; at most one is emitted.
2. **`Usage` precedes `Finish`** when present.
3. **EOF after an observed stop reason emits `Finish`.**
4. **No stop reason observed ⇒ no `Finish`.** Truncation is never reported as a
   clean stop.
5. **Cancellation emits no `Finish`.**
6. **A mid-stream error emits no `Finish`.**
7. **At most one `ToolCallDelta` per `call_id` carries `Some(name)`.**

Assertions 3 and 4 are conditioned on whether a stop reason was observed, which
the harness cannot infer from the emitted events — a provider that wrongly
suppresses `Finish` looks identical to one that correctly never saw a stop
reason. The condition is therefore a **property of the scenario, declared by the
suite**, not something read back from the subject: S1, S2, S5 and S7 encode a
stop reason in their wire script, S3 and S4 do not. That is what lets assertion
3 fail rather than pass vacuously.

**Assertion 2 is subsumed by assertion 1.** If nothing may follow `Finish`, any
`Usage` present necessarily precedes it. Both are kept, but 2 exists for its
diagnostic, not for coverage: it names the `[…, Finish, Usage]` shape of SMA-522
instead of reporting a generic "event after Finish". The spec states this so the
suite is not mistaken for seven independent properties.

### 7.1 Vacuous passes

The ticket names the trap in assertion 3: a stream that emits nothing satisfies
"ends with `Finish`" trivially. The same hazard applies to a miswired adapter
serving the wrong fixture, which would pass every assertion by producing
nothing.

Each scenario therefore carries a positive-evidence floor, checked before the
assertions run:

- S1–S5 must yield ≥ 1 `TokenDelta`.
- S6 and S7 must yield ≥ 1 `ToolCallDelta`.
- S1 and S7 must yield exactly one `Finish`.
- S4 must yield exactly one `Err`.

A stream that under-produces fails loudly rather than passing quietly.

## 8. Proving the suite can fail

The acceptance criteria ask that each assertion be verified to fail against a
deliberately broken translator. Doing that by hand once leaves nothing behind,
so it is built in instead.

`src/lib.rs` ships eight fake `Model` implementations, each violating exactly
one rule:

| Fake | Violates | Replicates |
| --- | --- | --- |
| `EventAfterFinish` | 1 | — |
| `DoubleFinish` | 1 (at-most-one) | — |
| `UsageAfterFinish` | 2 | SMA-522 |
| `NoFinishAfterStopReason` | 3 | SMA-531 |
| `FinishOnTruncation` | 4 | — |
| `FinishOnCancel` | 5 | — |
| `FinishAfterError` | 6 | — |
| `TwoNamedDeltas` | 7 | SMA-550 |

The crate's own unit tests assert the checker rejects each **with the specific
violation**, not merely that it errors. This runs on every CI run, so the suite
cannot silently decay into one that always passes.

## 9. Handling discovered failures

Default: **every provider passes every assertion when the PR merges.** Small,
obvious defects are fixed in this PR and noted in the PR body.

If a discovered defect needs a genuine design decision rather than a small fix,
implementation **stops and surfaces it** rather than deciding unilaterally. Only
then does it become an exception: a filed Linear issue, an
`#[ignore = "SMA-XXX: <one line>"]`, and a row in the suite's exception table.

Rationale: a suite that lands already-red teaches people to ignore it, which is
how the OpenAI bug shipped past green fixtures in the first place.

## 10. Documentation changes

All doc-only. The ticket's wording is used verbatim, including the load-bearing
"can detect" qualifier — without it, the two providers SMA-547 just fixed would
be non-conformant against a contract added in the same change.

| Site | Change |
| --- | --- |
| `core/src/model.rs` — `Model::invoke` | Add: implementations MUST emit `Finish` at end-of-stream when a stop reason was observed, and MUST NOT emit it on truncation with no stop reason observed, on cancellation, or after a mid-stream error. |
| `core/src/model.rs` — `ModelEvent::ToolCallDelta.name` | Replace "`Some` on the first delta only" (position) with the completeness wording (`Some` exactly once per `call_id`; buffer and concatenate fragments; never emit a name detectably incomplete). |
| `core/src/agent.rs` — `AgentEvent::ToolCallDelta.name` | Same replacement. |
| `bedrock/src/stream.rs:14` and `:439` | **SMA-532.** Both assert `Usage` "must precede" `Finish` per the ordering contract. Only `Finish` is positionally constrained. Comment text only; Bedrock's implementation is correct and is not touched. |
| `docs/book/src/concepts/model-providers.md:57` | Describes `Finish { reason }` as terminal; gains the emission rule alongside it, so prose and test cannot drift. |

No crate `README.md` changes: the suite is not a published crate, and no
published crate's public API, feature set, or install story changes.

## 11. Risks and fallbacks

| Risk | Fallback |
| --- | --- |
| Bedrock signing SigV4 against a local plain-HTTP endpoint with static credentials is unverified | Proven in the first implementation task. If it fails: `StaticReplayClient` for the six unpaced scenarios, S5 declined for Bedrock with a documented reason. |
| Hand-rolled chunked HTTP/1.1 may not satisfy every client (`reqwest`, `async-openai`, the AWS smithy client) | `hyper 1.11` and `hyper-util 0.1.20` are already in the lockfile; swap the server implementation, keep the interface. |
| SSE fixtures stored as `.txt` and `include_str!`'d break on Windows CI | Extend `.gitattributes` with a `text eol=lf` rule for the new fixture directory, as already done for the Anthropic fixtures. |
| The suite surfaces failures the ticket did not predict | §9. |

## 12. Out of scope

- Unifying the `finish()` return shape (§2.2) — follow-up ticket.
- SMA-548 (Anthropic `finish_or_error` reason-mapping rows with no assertion).
  That is one crate's mapping table having untested rows, not cross-provider
  ordering, and the ticket explicitly records it as not a duplicate.
- Any behavioural change to a provider beyond fixing a conformance failure the
  suite surfaces.

## 13. Acceptance criteria

1. All seven assertions run against all six subjects. Every declined scenario
   carries a printed reason; no assertion is skipped for any subject.
2. Each assertion has a non-conformant fake that the checker rejects with the
   specific violation, asserted in the suite's own unit tests (§8).
3. Every scenario enforces its positive-evidence floor (§7.1).
4. The three `-core` doc sites, the two Bedrock comment sites, and the book page
   carry the new wording (§10).
5. No `version` field is edited anywhere (§2.3).
6. The full CI gate list in CLAUDE.md passes: `fmt`, `clippy` with
   `-D warnings`, `cargo test --workspace --all-features`, `cargo doc` with
   `RUSTDOCFLAGS=-D warnings`, doc coverage, and `mdbook build docs/book`.

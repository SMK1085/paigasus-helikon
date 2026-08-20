# SMA-533 — Cross-provider conformance suite for stream event-ordering

**Status:** revised after adversarial challenge; pending approval
**Date:** 2026-08-19
**Branch:** `feature/sma-533-add-a-cross-provider-conformance-suite-for-stream-event`
**Ticket:** [SMA-533](https://linear.app/smaschek/issue/SMA-533/add-a-cross-provider-conformance-suite-for-stream-event-ordering)

## 1. Problem

Six streaming translators independently re-derive the event-ordering contract on
`paigasus_helikon_core::Model::invoke`, and nothing asserts it across them. Two
have already got it wrong, in different ways:

- **OpenAI** emitted `Finish` before `Usage` on every streaming turn (PR #197).
  It went undetected because the fixtures encoded a wire shape that does not
  occur.
- **Anthropic** emitted no `Finish` at all when a stream truncated between
  `message_delta` and `message_stop` (PR #200).

The six subjects are `openai/backend/chat.rs`, `openai/backend/responses.rs`,
`anthropic/stream.rs`, `bedrock/stream.rs`, `gemini/stream.rs` and
`litellm/stream.rs` — five crates, because OpenAI carries two backends.

### 1.1 Coverage delta — what is actually new

Not every scenario is novel for every subject. Four crates already test parts of
this at the `Model::invoke` boundary with wiremock; Bedrock tests none of it
(it has only `tests/live.rs` and `tests/schema_rewriter.rs`).

| Subject | Existing boundary coverage | Genuinely new here |
| --- | --- | --- |
| `anthropic` | S1/S2/S3 (`tests/messages_streaming.rs`, `fixtures/eof_after_message_delta.txt`, `eof_mid_content_block.txt`) | S4b, S5b, S7 |
| `openai/chat` | S1/S3, cancel-before-response | S2, S4b, S5a/b, S6, S7 |
| `openai/responses` | partial | S3, S4a, S7 |
| `gemini` | S1/S3 | S2, S4a/b, S5a/b, S7 |
| `litellm` | S1/S3, cancel-before-response, S6 | S2, S4a/b, S5a/b, S7 |
| `bedrock` | **none** | everything |

So the value of this work is, in order: **Bedrock coverage**, the S4b/S5b paths
nobody tests anywhere, and cross-provider uniformity. The implementation is
sequenced accordingly (§11) — Bedrock and the paced server first, because they
carry all the unproven assumptions.

## 2. Ticket corrections

Statements in the ticket that are stale or incomplete as of `origin/main`
`0bf5e759`, corrected so the implementation is not built on them.

**2.1 — The SMA-550 caveat no longer applies, but assertion 7 is not
exception-free.** The ticket's *Ordering vs SMA-550* section says
`providers-litellm` fails assertion 7 and that SMA-533 must land SMA-550 first
or scope the assertion knowingly. SMA-550 merged as `0bf5e759` (PR #208);
verified both by `git log origin/main --grep SMA-550` and by the merged source,
which canonicalizes correlation onto the resolved `call_id` and states that one
`call_id` owning exactly one state entry "is what makes 'at most one
name-carrying delta per `call_id`' structural"
(`litellm/src/stream.rs:25-29`). LiteLLM is conformant.

**`openai/chat` is not, for one shape** —
`openai/src/backend/chat.rs:400-417` documents this against itself:

> Given two deltas carrying different `index` values but the **same** `id` —
> malformed, since an `id` identifies a call — litellm merges them into one call
> emitting one name, while this translator keeps two indexes and emits a name
> for each: two name-carrying `ToolCallDelta`s for one `call_id`. […] That shape
> is unobserved from any backend […] A cross-provider conformance suite
> asserting "at most one name-carrying delta per `call_id`" would fail here and
> pass for litellm; closing it needs its own ticket.

That shape has **no capture anywhere in the repo** and is unobserved from any
backend, so under the provenance rule in §6 it is not in the fixture set and the
suite does not exercise it. Assertion 7 therefore stays green for all six
subjects without an `#[ignore]`. But the gap is real and must not be silently
inherited: it is recorded in §12 as a known-uncovered shape with its own
follow-up ticket. The earlier draft of this spec claimed assertion 7 needed "no
documented exception" — that was too strong.

**2.2 — The `finish()` return shape diverges four ways, not three.**

| Subject | `consume` returns | `finish()` returns |
| --- | --- | --- |
| `openai/backend/chat` | `Vec<ModelEvent>` | `Vec<ModelEvent>` |
| `openai/backend/responses` | `Result<Vec<ModelEvent>, ModelError>` | *(absent)* |
| `anthropic` | `Result<Vec<Result<ModelEvent, ModelError>>, ModelError>` | `Option<Result<ModelEvent, ModelError>>` |
| `bedrock` | `Vec<Result<ModelEvent, ModelError>>` | `Option<Result<ModelEvent, ModelError>>` |
| `gemini` | `Vec<Result<ModelEvent, ModelError>>` | `Vec<Result<ModelEvent, ModelError>>` |
| `litellm` | `Vec<ModelEvent>` | `Vec<ModelEvent>` |

**Decision: deferred to a follow-up ticket** (D3). The suite asserts at the
`Model::invoke` boundary and never observes `finish()`, so unifying it serves no
assertion here, while a five-crate signature refactor carries a different risk
profile from adding tests. This table is carried verbatim into the follow-up.

**2.3 — No manual version bumps.** The doc-only `-core` edit carries a patch
bump plus a dependent cascade, but it happens *automatically*. CLAUDE.md's
manual-bump ritual applies only when an ascending stub needs same-PR `-core` API
to survive `cargo publish --verify`. Nothing ascends here and the new member is
`publish = false` from birth. Because `-core` is already released, release-plz
performs the bump itself — the precise condition under which
`dependencies_update` (`release-plz.toml:10`) cascades. Hand-bumping would
*defeat* that cascade and strand the facade, the second-order caveat CLAUDE.md
documents. **Do not edit any `version` field.**

Note the cascade reaches **every** dependent of `-core`, not just the facade, so
the release PR will touch most of the workspace. Say so in the PR body.

## 3. Decisions taken

| # | Decision | Rationale |
| --- | --- | --- |
| D1 | Assert at the `Model::invoke` boundary against a mock transport | The contract is *stated* on `invoke`. A translator-only harness cannot catch SMA-531 — see §4. |
| D2 | Suite lives in `tests/provider-stream-conformance` | Follows `tests/runtime-http-conformance`; removes the dev-dep cycle rather than working around it — §5. |
| D3 | `finish()` shape deferred | §2.2 |
| D4 | Green on merge; escape hatch needs sign-off | A suite that lands red teaches people to ignore it. §9. |
| D5 | Fold in SMA-532 | Two doc comments in `bedrock/src/stream.rs` assert the exact misreading this PR's core wording exists to kill, in a file this suite now tests. |
| D6 | Server built on `hyper`, not raw TCP | §5.2 |
| D7 | The decline set is pinned and asserted | §9.1 |

## 4. Why the Model boundary, not the translator

Assertions 5 and 6 are properties of the **driver**, not the translator: on
cancellation and on a transport error the translator is never called at all.

More decisively, **SMA-531 was a driver defect.** Anthropic's translator
correctly buffered the stop reason; `model.rs:113` was a bare `None => return`
with no flush — and the crate had no `finish()` for a harness to call in the
first place (SMA-531's design doc, lines 20-21: "The crate's only `fn finish*`
is `finish_or_error` … that the driver never calls"). A harness that supplies
its own loop would have gone green against that exact defect. That is the
"conformance suite that cannot fail" failure mode the acceptance criteria name.

The six drivers are *similar* but not identical, and the differences matter for
fixture design:

- `openai/backend/responses.rs:70` — `None => return`, **no flush at all**.
- `gemini/src/model.rs:149-158` and `litellm/src/model.rs:163-166` — a `[DONE]`
  arm that flushes and returns, *plus* a separate EOF arm that also flushes.
  Two distinct terminal paths, which is what makes S2 non-vacuous for them.
- `bedrock/src/model.rs:102-124` — matches `Result<Option<_>, _>` from a
  channel receiver, not `Option<Result<_, _>>` from a stream.
- `gemini/src/model.rs:170-174` — returns mid-`consume` on the first `Err`.

The cost of the Model boundary is per-subject wire fixtures. That cost is
accepted.

## 5. Architecture

New workspace member `tests/provider-stream-conformance`, package name
`paigasus-helikon-provider-stream-conformance`, mirroring
`tests/runtime-http-conformance`:

```
tests/provider-stream-conformance/
├── Cargo.toml            version = "0.0.0", publish = false, [lints] workspace = true
├── src/lib.rs            Scenario, Outcome, checker, PacedServer, non-conformant fakes
│                         deps: paigasus-helikon-core, async-trait, futures-util,
│                               tokio, hyper, hyper-util, http-body-util
├── tests/conformance.rs  registers the six subjects, runs the table
│                         dev-deps: all five provider crates
└── fixtures/             per-subject wire scripts
```

Three registration chores that are easy to miss:

- `Cargo.toml:3` is `members = ["crates/*", "tests/runtime-http-conformance"]` —
  the `tests/` entries are **enumerated, not globbed**. Add the new path.
- `release-plz.toml` gets a `publish = false` / `release = false` block,
  matching the two existing internal members.
- `scripts/check-doc-coverage.sh:29-30` iterates every `cargo metadata
  --no-deps` package with only `paigasus-helikon-cli` excluded, and the
  precedent sets `[lints] workspace = true`. So every `pub` item in `src/lib.rs`
  — `Scenario`, `Outcome`, `assert_conforms`, the fakes — **needs `///` docs**
  or the required `docs` and `doc-coverage` gates fail.

**The dependency arrow points one way: suite → providers.** No provider crate
depends on the suite, so the dev-dependency cycle that deadlocked release-plz in
SMA-326 cannot arise. This is the main reason to prefer the
`runtime-http-conformance` precedent over the `sessions-testkit` one the ticket
names — `sessions-testkit` *is* dev-depended by its three backends, which is
exactly the cycle shape.

Placing it under `tests/` also leaves the crate roster, the facade and every
README untouched. `docs/book/src/getting-started/workspace-layout.md` scopes its
"21" to `crates/` (there are exactly 21 directories there, and
`runtime-http-conformance` already sits outside the count), so that sentence
stays true and needs no edit.

### 5.1 The seam

`src/lib.rs` knows nothing about any provider:

```rust
pub enum Scenario { /* §6 */ }

/// What a subject did with a scenario. Declining is a first-class outcome with
/// a mandatory reason, not an `Option` a caller can silently treat as a skip.
pub enum Outcome {
    Served {
        stream: BoxStream<'static, Result<ModelEvent, ModelError>>,
        /// Fires once the client has observably consumed the gate event.
        /// Present only for the cancellation scenarios. §6.1.
        gate: Option<GateHandle>,
    },
    /// The wire shape cannot physically occur for this provider.
    Declined(&'static str),
}

#[async_trait]
pub trait StreamUnderTest {
    /// e.g. "openai/chat". Used in failure output and the decline set.
    fn name(&self) -> &'static str;

    /// Whether this subject's fixture for `scenario` encodes a stop reason.
    /// Cross-checked against the suite's own expectation — §6.2.
    fn encodes_stop_reason(&self, scenario: Scenario) -> bool;

    /// The tool name this subject's S6/S7 fixture declares, for the §7.1 floor.
    fn fixture_tool_name(&self) -> &'static str;

    async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome;
}

pub async fn assert_conforms(subject: &impl StreamUnderTest);
```

### 5.2 The paced server

wiremock writes the whole body in a single `set_body_raw`, so there is no pacing
to interrupt. `litellm/tests/cancellation.rs:3-5` documents this as the reason a
true mid-stream cancel goes untested, and OpenAI's tests carry the same note.
That limitation makes assertion 5 untestable in any meaningful sense:
cancel-*before*-response is satisfied trivially by any implementation.

**The server is built on `hyper` 1.11 + `hyper-util` + `http-body-util`**, not a
hand-rolled `TcpListener`. A raw listener must also drain the request body
before responding (or a larger request races an RST), handle `Expect:
100-continue`, and manage keep-alive and protocol negotiation — none of which
serve the design, and all of which are failure modes that would present as
provider bugs. A `service_fn` with a channel-backed body gives correct chunked
framing for free and, critically, an **abortable** body that surfaces reliably
as an `Err` in reqwest, `async-openai` and the smithy client.

The server emits a `Vec<Vec<u8>>` of chunks with a release gate between them.
Being byte-oriented rather than SSE-oriented, one server serves all five crates:
`text/event-stream` for OpenAI, Anthropic, Gemini and LiteLLM, and
`application/vnd.amazon.eventstream` frames for Bedrock.

Reachability: `base_url()` on OpenAI, Anthropic, Gemini and LiteLLM;
`SdkConfig::endpoint_url` plus static test credentials on Bedrock (unproven —
§11).

`hyper`, `hyper-util`, `http-body-util` and `aws-smithy-eventstream` are all
already in `Cargo.lock` transitively, but "in the graph" is not "a dependency":
using them directly requires new `[workspace.dependencies]` pins per CLAUDE.md's
single-source-of-truth rule, and each adds a Dependabot surface.

## 6. Scenarios

| # | Scenario | Wire script | Asserts |
| --- | --- | --- | --- |
| S1 | `CleanStop` | deltas → stop-reason chunk → **usage chunk** → terminator → clean EOF | 1, 2, 3 |
| S2 | `TruncatedAfterStopReason` | deltas → stop reason observed → body ends cleanly, no terminator | 3 |
| S3 | `TruncatedMidGeneration` | deltas → body ends cleanly, no stop reason ever | 4 |
| S4a | `ErrorMidGeneration` | deltas → body aborted, no stop reason | 6, 1 |
| S4b | `ErrorAfterStopReason` | deltas → **stop reason observed** → body aborted | 6, 1 |
| S5a | `CancelMidGeneration` | deltas → gate → token fires, no stop reason | 5 |
| S5b | `CancelAfterStopReason` | deltas → **stop reason observed** → gate → token fires | 5 |
| S6 | `FragmentedToolName` | tool-call deltas, name split across ≥2 fragments | 7, 1 |
| S7 | `ToolCallCleanStop` | one complete tool call → tool-use stop reason → terminator | 1, 3, 7 |

**S4 and S5 are each split, and the split is the point.** The single-scenario
versions in the earlier draft could not fail. With no stop reason buffered, a
driver that *wrongly* flushes `finish()` on the error arm emits no `Finish`
either — there is nothing to flush — so assertion 6 passed for correct and
broken code alike. S4b supplies the buffered stop reason that makes the
assertion bite. This is not hypothetical: SMA-522's design doc records
`Transport / parse error after finish chunk` as a changed path, and SMA-531's
design doc (lines 78-81) names "SMA-533's acceptance clause 6" as its reason for
leaving Anthropic's dirty-cut arm unguarded. **This ticket is the one that was
supposed to guard it.** The same reasoning splits S5.

**S1 must place usage *after* the stop reason.** That is the real OpenAI wire
order. SMA-522 went undetected precisely because the fixtures encoded a shape
that does not occur.

**S3 and S4a differ only in how the body ends.** S3 completes the body cleanly:
the client sees EOF mid-generation, no stop reason was observed, no `Finish` may
be emitted. S4a aborts it: the client sees a truncated body, yields `Err`, and
still emits no `Finish`. `async-openai`'s pump treats a clean EOF as `None`
rather than an error (`client.rs:842-865`), so S3 will not be mis-reported as
S4a.

**Provenance rule.** Every fixture is transcribed from captured or
already-committed traffic — the repo has fixture directories for LiteLLM
(captured against 1.97/1.98 with the image digest recorded in the file header),
Anthropic and OpenAI — and never invented from vendor documentation. Applying
that rule to this spec's own first draft: S6 originally specified "id resolving
after the first fragment", which **no capture in the repo supports**. The one
committed capture of a fragmented name,
`litellm/tests/fixtures/tool_call_stream_fragmented_name.txt`, states in its
header that the fragments "BOTH arrive after the id". S6 uses the committed
shape; the late-id variant is dropped.

### 6.1 The cancellation gate

A cancellation test without a happens-before edge is sleep-and-hope —
`openai/tests/cancellation.rs:88-93` explicitly refuses to ship one, on the
grounds that it "would assert whatever the scheduler happened to do".

The stop-reason chunk itself is not usable as that edge: `openai/chat` buffers
`finish_reason` silently (`backend/chat.rs:290-314`), as does LiteLLM. So S5b's
script places an **observable** event after the stop reason and before the gate
— a usage chunk for `openai/chat`, `litellm` and `gemini`; for Anthropic
`message_delta` already emits `Usage` from the same event
(`anthropic/stream.rs:161-181`). The harness blocks on observing that event,
then fires the token. `GateHandle` (§5.1) is what the subject hands back to let
the harness release the remaining chunks.

### 6.2 Declines, and why they are cross-checked

| Subject | S1 | S2 | S3 | S4a | S4b | S5a | S5b | S6 | S7 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `openai/chat` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `litellm` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `anthropic` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ a | ✓ |
| `gemini` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ a | ✓ |
| `bedrock` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ b | ✗ a | ✓ |
| `openai/responses` | ✓ | ✗ c | ✓ | ✓ | ✗ c | ✓ | ✗ c | ✗ a | ✓ |

- **a** — the tool name arrives whole in a single `content_block_start` /
  `functionCall` / `toolUse` / `output_item.added` event. There is no fragment
  to split.
- **b** — the window between "stop reason buffered" and "`Finish` emitted" lies
  strictly between `MessageStop` and `Metadata`, with no event emitted in
  between (`bedrock/src/stream.rs:214-242`), so no gate edge exists.
- **c** — `terminal_events` builds `Usage` and `Finish` from one upstream event
  (`backend/responses.rs:454-489`), so "stop reason observed but no `Finish`
  yet" is not a reachable state.

Every assertion still runs for every subject: 5 via S5a where S5b is declined,
6 via S4a where S4b is declined, 7 via S7 where S6 is declined.

**The stop-reason condition is cross-checked, not merely declared.** Assertions
3 and 4 are conditioned on whether a stop reason was observed, and the harness
cannot infer that from the emitted events — a provider wrongly suppressing
`Finish` looks identical to one that correctly never saw a stop reason. The
earlier draft declared the condition suite-side while each subject supplied its
own bytes, with nothing reconciling the two; a mis-transcribed fixture would
then make assertion 3 pass vacuously. So `encodes_stop_reason` (§5.1) is
declared per subject and **asserted against the suite's own expectation for that
scenario**, and S1/S7 additionally assert the specific `FinishReason` the
fixture declares.

## 7. Assertions

1. **`Finish`-terminated.** Nothing follows `Finish` — no `ModelEvent` and no
   `Err`; at most one `Finish` is emitted.
2. **`Usage` precedes `Finish`** when present.
3. **EOF after an observed stop reason emits `Finish`.**
4. **No stop reason observed ⇒ no `Finish`.**
5. **Cancellation emits no `Finish`.**
6. **A mid-stream error emits no `Finish`.**
7. **Exactly one `ToolCallDelta` per observed `call_id` carries `Some(name)`,
   and that name equals the fixture's declared tool name.**

**Assertion 7 says "exactly", not "at most".** The ticket's wording is "at most
one", which a translator that drops the name entirely satisfies — and that is
precisely SMA-547's bug class. `openai/backend/chat.rs:338-349` states the
stakes: the name flush "is a correctness requirement, not a diagnostic: … a name
never emitted becomes an empty name that resolves to no tool."

**Assertion 2 is subsumed by assertion 1.** If nothing may follow `Finish`, any
`Usage` present necessarily precedes it. Both are kept, but 2 exists for its
diagnostic, not for coverage.

**Violation classification is ordered**, because the rules overlap and §8 demands
a *specific* violation. The checker reports the first match in this order:

1. a second `Finish` → `DuplicateFinish` (assertion 1)
2. a `Usage` after `Finish` → `UsageAfterFinish` (assertion 2)
3. any other event or `Err` after `Finish` → `EventAfterFinish` (assertion 1)
4. assertions 3–7 in numeric order

### 7.1 Vacuous passes

A stream that emits nothing satisfies "ends with `Finish`" trivially, and a
miswired adapter serving the wrong fixture would pass every assertion by
producing nothing. Each scenario therefore carries a positive-evidence floor,
checked *before* the assertions run:

- S1–S5b must yield ≥ 1 `TokenDelta`.
- S6 and S7 must yield ≥ 1 `ToolCallDelta`, of which **exactly one carries
  `Some(name)` equal to `fixture_tool_name()`**.
- S1 and S7 must yield exactly one `Finish`, with the `FinishReason` the fixture
  declares.
- S4a and S4b must yield exactly one `Err`.

Every scenario also runs under a `tokio::time::timeout`. A subject whose stream
never terminates is a real bug this suite should catch, and without the timeout
it would hang `cargo test` rather than fail it.

## 8. Proving the suite can fail

Verifying by hand once leaves nothing behind, so it is built in. `src/fakes.rs`
ships ten non-conforming fake event sequences (plus `Fake::Conforming`, which
violates none), each violating exactly one rule, and the crate's own unit
tests assert the checker rejects each **with the classification named in
§7**. These are deliberately *not* `Model` implementations: each fake builds
its `Vec<Result<ModelEvent, ModelError>>` in memory and calls `classify`
directly, so no server, no transport and no provider crate is on this path —
a fake can never be rejected by a floor instead of the classification it is
written to trigger (`src/fakes.rs`'s module doc explains and justifies this):

| Fake | Expected classification | Replicates |
| --- | --- | --- |
| `EventAfterFinish` | `EventAfterFinish` | — |
| `ErrAfterFinish` | `EventAfterFinish` | — |
| `DoubleFinish` | `DuplicateFinish` | — |
| `UsageAfterFinish` | `UsageAfterFinish` | SMA-522 |
| `NoFinishAfterStopReason` | assertion 3 | SMA-531 |
| `FinishOnTruncation` | assertion 4 | — |
| `FinishOnCancel` | assertion 5 | — |
| `FinishAfterError` | assertion 6 | — |
| `TwoNamedDeltas` | assertion 7 | SMA-550 |

A tenth case, `NoNamedDelta`, guards the "exactly one" tightening: a translator
that emits tool-call deltas but never a name must fail assertion 7.

This runs on every CI run, so the suite cannot silently decay into one that
always passes.

## 9. Handling discovered failures

Default: **every subject passes every assertion when the PR merges.** Small,
obvious defects are fixed here and noted in the PR body.

If a discovered defect needs a genuine design decision rather than a small fix,
implementation **stops and surfaces it** rather than deciding unilaterally. Only
then does it become an exception: a filed Linear issue, an
`#[ignore = "SMA-XXX: <one line>"]`, and a row in the exception table.

### 9.1 The decline set is pinned

`Outcome::Declined` would otherwise be a second escape hatch with none of that
rigor — a future engineer facing a red assertion could convert it to
`Declined("wire shape cannot occur")` in one line and keep CI green.

So the expected declines are pinned as a `const DECLINED: &[(&str, Scenario, &str)]`
in `src/declines.rs` — subject, scenario, and the reason string, so a reworded
reason is drift too — exactly matching §6.2, and the suite **fails when the observed
decline set differs in either direction** — an unexpected decline *and* an
expected decline that stopped happening. Adding or removing one then requires a
reviewed diff to a table, not a string literal in a match arm.

## 10. Documentation changes

All doc-only. **Four** `-core` sites, not three: `ModelEvent::ToolCallDelta`
carries the positional wording twice, once on the variant and once on the
`name` field.

| Site | Change |
| --- | --- |
| `core/src/model.rs` — `Model::invoke` | Add the emission rule (exact text below). |
| `core/src/model.rs:180-182` — `ToolCallDelta` **variant** doc | "`name` is `Some` on the first delta for a given `call_id`, then `None` on subsequent deltas" → completeness wording. |
| `core/src/model.rs:186` — `ToolCallDelta.name` **field** doc | "Tool name; `Some` on the first delta only." → completeness wording. |
| `core/src/agent.rs:384` — `AgentEvent::ToolCallDelta.name` | Same replacement. |
| `bedrock/src/stream.rs:14` and `:439` | **SMA-532.** Both assert `Usage` "must precede" `Finish` per the ordering contract. Only `Finish` is positionally constrained. Comment text only; Bedrock's implementation is correct and is not touched. |
| `docs/book/src/concepts/model-providers.md:57` | Describes `Finish { reason }` as terminal; gains the emission rule, so prose and test cannot drift. |

**Exact text, `Model::invoke`:**

> Implementations MUST emit `Finish` at end-of-stream when a stop reason was
> observed, and MUST NOT emit it on truncation with no stop reason observed, on
> cancellation, or after a mid-stream error.

**Exact text, all three `ToolCallDelta.name` sites:**

> `Some` exactly once per `call_id`, on the first delta for which the provider
> can establish the name is complete, and `None` on every other delta. When
> `Some`, the value is the whole name so far as the provider can determine — a
> provider receiving the name in fragments MUST buffer and concatenate them, and
> MUST NOT emit a name it can detect is still incomplete.

The **"can detect" qualifier is load-bearing** and must survive verbatim: a
single delta carrying both `{"name":"get_","arguments":"{\"ci"}` flushes
`Some("get_")`, a partial no translator can rule out without abandoning
streaming names entirely. An unqualified "never emit a partial" would make the
two providers SMA-547 just fixed non-conformant against a contract added in the
same change.

**This tightens a public trait's contract for third-party implementors** under a
patch bump. It is doc-only and semver-compatible, but the CHANGELOG entry and
the book page should describe it as a contract clarification, not a doc tweak.

No crate `README.md` changes: the suite is not a published crate, and no
published crate's public API, feature set or install story changes.

## 11. Risks, fallbacks, and sequencing

Bedrock and the paced server carry every unproven assumption and no existing
coverage (§1.1), so they are built **first**. If they cannot be made to work the
design changes shape, and that must be known before five subjects are written
against it.

| Risk | Fallback |
| --- | --- |
| SigV4 against a local plain-HTTP endpoint with static credentials is unverified. The smithy TLS builder is `.https_or_http()` but also calls `.enable_http2()`. | Proven by execution in the first task, per this repo's rule that wire behaviour is verified against a running server rather than read from docs. If it fails: `StaticReplayClient` covering **S1, S2, S3, S6, S7 only** — a whole-body replay client cannot abort a body, so S4a/S4b and S5a/S5b are declined for Bedrock with reasons. Note that `StaticReplayClient` sits behind `aws-smithy-runtime`'s `test-util` feature, which `Cargo.toml:142` does not currently enable, so this also shifts feature unification under `--all-features`. |
| Hand-built eventstream frames may decode as `Unknown` if `:event-type` must match the union member name exactly or `:content-type` must be `application/json`. `bedrock/src/stream.rs:244-246` has a forward-compat catch-all that **silently drops** unknown variants, so this would present as every Bedrock scenario failing its positive-evidence floor — looking like a translator bug. | Assert the floor first, so the failure is attributed correctly; validate one frame round-trips before writing the rest. |
| Bedrock's registration is "roughly twenty lines" for the other five subjects but well past it here: `SdkConfig` + `endpoint_url` + test credentials, a frame writer with `:message-type` / `:event-type` / `:content-type` headers, and per-event JSON payloads. | Budgeted as its own task. |
| Bedrock's eventstream fixtures are **binary**. A `*.txt text eol=lf` glob over them would silently corrupt them. | Add an explicit `binary` (`-text`) rule for the Bedrock fixture directory in `.gitattributes`, alongside the existing `text eol=lf` rule for SSE fixtures (`.gitattributes:3-15`). |
| The suite surfaces failures the ticket did not predict | §9. |

## 12. Out of scope, with follow-ups to file

- **Unifying the `finish()` return shape** (§2.2). Carry the table verbatim.
- **`openai/chat`'s same-`id`-different-`index` shape** (§2.1) — real, unobserved
  from any backend, uncovered by this suite because no capture exists. The
  in-source comment already says "closing it needs its own ticket"; file it.
- **`paigasus_helikon_evals::MockModel` ignores its `_cancel` argument**
  (`evals/src/mock.rs:48-62`), so a `Model` shipped by this workspace violates
  the *existing* written cancellation clause and would fail assertion 5. Not a
  provider, so out of this suite's scope — but it is a conformance defect in
  code we own, found while writing this spec. File it.
- **SMA-548** (Anthropic `finish_or_error` reason-mapping rows with no
  assertion) — one crate's mapping table, not cross-provider ordering; the
  ticket records it as not a duplicate.
- **`ReasoningDelta` ordering, parallel tool calls, zero-argument tool calls.**
  No scenario covers them. `Err` terminality *is* covered, folded into
  assertion 1.
- Any behavioural change to a provider beyond fixing a conformance failure the
  suite surfaces.

## 13. Acceptance criteria

1. Every (subject, scenario) pair either runs or is `Declined` with a reason
   drawn from the pinned `DECLINED` set (§9.1), and the suite fails if the
   observed decline set differs from it in either direction.
2. Every assertion 1–7 runs for all six subjects, via the substitutions in
   §6.2 where a scenario is declined.
3. Each assertion has a non-conformant fake that the checker rejects with the
   classification named in §7, asserted in the suite's own unit tests (§8) —
   including `NoNamedDelta` for the "exactly one" tightening.
4. Every scenario enforces its positive-evidence floor and runs under a timeout
   (§7.1).
5. The four `-core` doc sites, the two Bedrock comment sites and the book page
   carry the wording quoted verbatim in §10.
6. No `version` field is edited anywhere (§2.3).
7. Follow-up tickets from §12 are filed and linked in the PR body.
8. The full CI gate list in CLAUDE.md passes: `fmt`, `clippy -D warnings`,
   `cargo test --workspace --all-features`, `cargo doc` with
   `RUSTDOCFLAGS=-D warnings`, doc coverage, and `mdbook build docs/book`.

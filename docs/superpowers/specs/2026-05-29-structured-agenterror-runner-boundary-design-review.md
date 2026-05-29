# SMA-346 Design Review — Surface structured `AgentError` at the Runner boundary

**Reviews:** [`2026-05-29-structured-agenterror-runner-boundary-design.md`](./2026-05-29-structured-agenterror-runner-boundary-design.md)
**Reviewer perspective:** staff engineering — fitness against the planned design and downstream blast radius
**Date:** 2026-05-29
**Verdict:** **Changes requested — one blocking correctness bug.** The approach (out-of-band `FailureSlot`, `AgentEvent` left string-based, no `Agent`-trait change) is the right shape and low blast radius, and I verified its dependencies are real now that SMA-321 is merged. But as written the side-channel **does not deliver the structured error for state-machine failures** (`MaxTurnsExceeded`, `NotImplemented`, and `InvalidStructuredOutput` via `collect()`) — i.e. the exact taxonomy the ticket exists to expose. The recording happens *after* the `RunFailed` event is yielded, while `collect()` reads the slot *at* that event and returns. Fix **H1** before the plan; the rest are minor.

## What this was checked against

- **Linear** [SMA-346](https://linear.app/smaschek/issue/SMA-346) (problem + constraint) and its parent [SMA-313](https://linear.app/smaschek/issue/SMA-313) (why `RunFailed` is string-based).
- **Code (ground truth, current `main` with SMA-320 + SMA-321 merged)** — `crates/paigasus-helikon-core/src/{agent.rs, runner.rs, context.rs, loop_state.rs}`, `crates/paigasus-helikon-runtime-tokio/src/lib.rs`, root `Cargo.toml`. Every load-bearing claim was verified against source.

Severity legend: **H** = high / blocking · **M** = medium · **N** = minor / nit. Each item ends with a concrete **Correction**.

---

## H — High-severity (blocking)

### H1. The slot is written *after* `RunFailed` is yielded, but `collect()` reads it *at* `RunFailed` and returns — so state-machine failures lose their structure

This is the core mechanism, and it doesn't work for the cases that motivate the ticket. Three verified facts combine:

1. **`collect()` early-returns on `RunFailed`** (`runner.rs`). It does not drain to the end:

   ```rust
   crate::AgentEvent::RunFailed { error } => {
       let err_msg = error.clone();
       events.push(ev);
       return Err(RunError::Other(anyhow::anyhow!(err_msg)));   // ← returns here
   }
   ```

2. **The driver yields all events (including `RunFailed`) *before* it runs the `NextAction::Terminate` arm** (`agent.rs`):

   ```rust
   let TransitionOutcome { next_state, events, next_action, conversation_appends } = outcome;
   for ev in events { yield ev; }     // ← RunFailed handed to the consumer here; generator suspends
   loop_state = next_state;
   match next_action {
       NextAction::Terminate => return,   // ← spec inserts `failure.set(err)` here — AFTER the yield
       …
   }
   ```

3. **The spec records state-machine failures in that `Terminate` arm** (§3), and even notes *"events are still yielded before this point."*

Put together: when the agent yields `RunFailed`, `async_stream` suspends the generator. `collect()` receives `RunFailed`, reads the slot, and **returns immediately** — it never polls again, so the generator never resumes, so `failure.set(err)` in the `Terminate` arm **never runs**. The slot is empty at read time → `collect()` falls back to `RunError::Other(string)`. This happens for every failure routed through `LoopState::Failed` + `Terminate`: **`MaxTurnsExceeded`, `NotImplemented`, and `InvalidStructuredOutput`** (via the non-typed `collect()`).

By contrast the **three direct sites work** — they `set` *before* the `yield` (`failure.set(...); yield RunFailed; return;`), so the slot is populated when `collect()` reads it. That asymmetry is the tell: the design is correct for the 3 direct sites and broken for the 3 state-machine sites, so §3's claim of *"all six failure pathways with full fidelity"* does not hold as written.

This also contradicts the spec's own test plan — *"max-turns → `RunError::Agent(AgentError::MaxTurnsExceeded(..))`"* would **fail** against this design (it'd produce `RunError::Other`). Note the internal inconsistency: `FailureSlot`'s doc and the cited `controlled()` precedent describe *"read once after draining,"* but `collect()`/`collect_typed()` read the slot *at* the `RunFailed` event, not after draining — fine for set-before-yield sites, wrong for the `Terminate`-arm sites.

**Correction — pick one (A preferred):**

- **(A) Make the slot read genuinely "after draining."** In `collect()`/`collect_typed()`, on `RunFailed` record the error string and **keep polling** until the stream ends (`None`), *then* read the slot and decide `Agent(err)` vs `Other(string)`. After the `RunFailed` yield the generator resumes, runs the `Terminate`-arm `set`, and returns `None` — so the slot is populated by the time you read it. This matches the `FailureSlot` doc and the `controlled()` precedent, keeps §3 as written, and is robust against future failure sites that forget the ordering. Cost: one extra poll on the failure path (the stream ends right after `RunFailed` anyway).
- **(B) Set before the yield for state-machine failures too.** Move the `set` out of the `Terminate` arm to *before* the `for ev in events` loop when `next_state` is `LoopState::Failed(_)` (extract the `AgentError` out of `next_state`, `set`, then yield, then `return`) — mirroring the three direct sites. Keeps `collect()`'s early-return, but every present and future failure yield must remember to precede itself with a `set`.

Either way, reconcile §3 (recording site) and §4 (read timing) so they agree; right now they don't.

---

## M — Medium

### M1. `InvalidStructuredOutput` is now carried by two mechanisms — confirm which wins and why

`collect_typed()` already reconstructs `InvalidStructuredOutput` from the dedicated `AgentEvent::StructuredOutputFailed` event, which is emitted *immediately before* `RunFailed` — so it's seen before the early-return and **works today**. SMA-346 adds the slot as a *second* carrier for the same error (§4: "prefer the slot … keep the `StructuredOutputFailed` reconstruction as the no-slot fallback"). Given H1, the slot is empty for `InvalidStructuredOutput`, so `collect_typed` silently uses the event fallback — meaning the slot contributes nothing here and the working path is the pre-existing event. That's not wrong, but it's worth being explicit: the `StructuredOutputFailed`-before-`RunFailed` pattern is exactly the ordering discipline H1 needs, and it already works. Consider whether the slot should carry `InvalidStructuredOutput` at all, or whether the cleaner unification is to generalize the "emit a structured-detail event before `RunFailed`" pattern (which sidesteps H1 entirely) rather than introduce a parallel out-of-band channel.

**Correction.** State the precedence and the redundancy explicitly. If you keep the slot, make sure the InvalidStructuredOutput slot value and the `StructuredOutputFailed` event can't disagree; if you adopt fix (A) the slot will actually populate and become the primary, so verify it equals the event-derived value.

### M2. Confirm `AgentError: Send + Sync` so the slot doesn't regress `RunContext`/stream `Send`

`FailureSlot(Arc<Mutex<Option<AgentError>>>)` must be `Send + Sync` for `RunContext` to stay `Send + Sync` and for the agent's `BoxStream<'static, AgentEvent>` to remain `Send`. `Arc<Mutex<Option<AgentError>>>: Send + Sync` requires `AgentError: Send`, which requires every payload — `ModelError`, `ToolError`, `SessionError`, `anyhow::Error`, `GuardrailKind` — to be `Send`. `anyhow::Error` is; the rest are almost certainly `thiserror` types that are, but this hasn't been asserted anywhere.

**Correction.** Add a `const _: fn() = || { fn assert<T: Send + Sync>() {} assert::<FailureSlot>(); };` (or a trybuild/`static_assertions`) so a future non-`Send` payload added to `AgentError` fails the build loudly rather than breaking the agent stream's `Send` bound in a confusing downstream error.

### M3. `lock().unwrap()` on a poisoned mutex

`set`/`take` use `.lock().unwrap()`. The critical sections are trivial (no panic while holding), so poisoning is unlikely, but a panic anywhere holding the lock turns into a second panic at the boundary. Low risk; acceptable for MVP, but a one-line note (or `lock().unwrap_or_else(|e| e.into_inner())`) would harden it.

---

## N — Minor / nits

### N1. Construction-site count is now 9, not 7

The spec says adding the field "needs no changes to any of the 7 construction sites." Verified that the *claim* holds — `RunContext::new` initializes `run_config: None` internally and the new `failure` field is defaulted the same way, so no call site changes — but there are now **9** `RunContext::new` call sites (SMA-321 added four in `runtime-tokio/tests/common`). Trivial; just update the number so it doesn't read as stale.

### N2. `FailureSlot` `Debug` is correctly optional — for the right reason

The spec says a manual `Debug` "may be added … not required." Confirmed correct: `RunContext` does **not** derive `Debug` (or `Clone`), so adding a non-`Debug` field doesn't break a derive. Worth stating *that's why* it's optional, since the usual reason a field must be `Debug` (a `#[derive(Debug)]` on the container) doesn't apply here.

### N3. Release sequencing is accurate — minor `feat`-vs-`patch` caveat

Verified: core is at **0.2.1**, the `[workspace.dependencies]` pin is `0.2.1`, and the same-PR-core-bump path is exactly the CLAUDE.md "ascending crate uses same-PR core API" caveat. The "0.2.1 → 0.2.2" target is right. One subtlety: adding new public items (`FailureSlot`, `with_failure`, `failure_handle`) is conventionally a `feat` (which release-plz would classify as a *minor* bump), whereas the spec plans a *patch*. CLAUDE.md explicitly sanctions "patch for additive" here, so this is fine — just be aware the manual patch bump and release-plz's commit-type classification need to agree (commit the core change in a way that doesn't make release-plz compute a conflicting bump).

### N4. Dependency facts now check out (no action — recorded for the reader)

The spec's present-tense claims that `TokioRunner::controlled()` "already uses an `Arc<Mutex<Outcome>>`" and that runtime-tokio is "already released" are **now true**: SMA-321 is merged, `controlled()` / `Outcome` / `OutcomeHandle::get()` / `finalize()` all exist as production code, `RunContext` already carries `run_config`, and `RunError` already has `Agent`/`Cancelled`/`Timeout`. The §5 wiring (`outcome.get()` → `Outcome::{Completed,Cancelled,TimedOut}`) matches the real API. So this spec builds on landed work, not a parallel design — the earlier "depends on unbuilt SMA-321" risk is closed.

---

## Verified OK (checked, no action needed)

- **`collect_typed` and `AgentEvent::StructuredOutputFailed` exist** (`runner.rs`, `agent.rs`) — the spec's "existing reconstruction / fallback" references are accurate, not aspirational.
- **`AgentError` taxonomy matches** the spec exactly: `Model(ModelError)`, `Tool(ToolError)`, `Session(SessionError)`, `Guardrail{kind}`, `InvalidStructuredOutput{schema_errors, final_text}`, `MaxTurnsExceeded(u32)`, `NotImplemented{feature}`, `Other(anyhow::Error)`; enum is `#[non_exhaustive]`.
- **Neither `AgentError` nor `RunError` is `Clone`** — fine, because the slot moves the value out via `take()` and `RunError::Agent(err)` takes ownership; no `Clone` is required on this path.
- **`to_tool_context()` excluding the slot** is consistent with the `run_config` treatment and correct (tools don't record terminal run failures).
- **The "don't touch `AgentEvent`" constraint holds** — the 16 serde-roundtrip snapshots and `Clone` on `AgentEvent` are untouched; the structured value rides entirely out-of-band. Good call.
- **`finalize`-after-`collect` on all paths** is already how the merged `TokioRunner` is structured, so the §5 wiring (`collect().await; finalize(&session).await; match outcome`) drops in cleanly and preserves the finalize-always guarantee.

---

## Required before writing the plan

1. **H1** — reconcile the recording site (§3, `Terminate` arm, after the yield) with the read timing (§4, at `RunFailed`, early-return). As written the slot is empty for `MaxTurnsExceeded` / `NotImplemented` / `InvalidStructuredOutput`-via-`collect()`. Adopt fix (A) (drain-then-read in `collect`/`collect_typed`) or fix (B) (set-before-yield for state-machine failures), and add the `max_turns → RunError::Agent(MaxTurnsExceeded)` test that currently would fail.

Recommended alongside: **M1** (resolve the `InvalidStructuredOutput` double-carrier), **M2** (`Send + Sync` static assertion on `FailureSlot`). The rest are nits.

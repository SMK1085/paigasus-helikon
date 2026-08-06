# SMA-482 — HTTP runtime hardening: 5xx redaction, session–principal binding, in-flight run cap

**Date:** 2026-08-06
**Linear:** [SMA-482](https://linear.app/smaschek/issue/SMA-482/runtime-axum-runtime-actix-harden-5xx-redaction-session-principal)
**Branch:** `feature/sma-482-runtime-axum-runtime-actix-harden-5xx-redaction-session`
**Crates:** `paigasus-helikon-runtime-axum`, `paigasus-helikon-runtime-actix`, `paigasus-helikon-runtime-http-conformance` (internal)

## Problem

CodeRabbit raised three hardening items on PR #173 (SMA-343, the actix port) and they were
deliberately deferred: each is a breaking or feature-level change, not a port defect, and PR
#173's premise was that the two runtimes stay byte-compatible.

1. **CWE-209, information disclosure.** `ServerError::Internal` and `ServerError::RunStart`
   serialise `self.to_string()` into the 500 response body, so underlying runner and WebSocket
   error text reaches an external caller.
2. **CWE-639, IDOR.** Session affinity keys solely on the caller-supplied `X-Session-Id` header.
   `InMemorySessionProvider` uses it as its only map key and `SessionLocks` as its only lock key.
   Any admitted caller who learns or guesses another caller's id can read and append to that
   conversation.
3. **CWE-770, unbounded resource consumption.** `RunRegistry::create` inserts unconditionally;
   `max_runs` caps only *retained terminal* runs. Concurrent live runs grow without bound.

All three apply identically to both HTTP runtimes. They share one constraint: **any fix must land
in both runtimes in the same change**, or it silently breaks the wire/API parity that
`tests/runtime-http-conformance` exists to assert.

## Goals

- Close all three findings in `runtime-axum` and `runtime-actix` with no behavioural divergence.
- Keep the redacted detail — route it to `tracing` rather than dropping it.
- Extend the conformance suite so the new behaviours are *asserted* across runtimes, not assumed.
- Carry the breaking API change with CHANGELOG entries and a migration note.

## Non-goals

Explicitly out of scope for this change; each is recorded in "Follow-ups" below.

- Authorising the WebSocket events endpoint against the principal.
- Bounding `X-Session-Id` length or character set.
- A per-principal sub-cap on in-flight runs.
- Any change to `paigasus-helikon-core`.

## Architecture

Three independent changes, applied symmetrically to both runtimes. The two crates have
structurally identical call sites — `handlers/runs.rs` resolves the session, acquires the
per-session lock, then calls `registry.create` — so each change is the same edit twice, modulo
each framework's request type.

No `paigasus-helikon-core` change is involved, so the same-PR core-bump caveat in `CLAUDE.md`
does not apply. Both runtime crates are already released, so release-plz performs the version
bumps through its normal flow (breaking change on a 0.x crate → minor bump: axum `0.1.5` →
`0.2.0`, actix `0.1.0` → `0.2.0`) and `dependencies_update` cascades the facade. No manual
version ritual is required in this PR.

---

## 1. Redact internal detail from 5xx response bodies

### Rule

Stated so it can be audited at a glance: **the two 500 variants are redacted; nothing else is.**

| Variant | Status | Body |
|---|---|---|
| `ServerError::Internal(_)` | 500 | `{"error":"internal error"}` |
| `ServerError::RunStart(_)` | 500 | `{"error":"internal error"}` |
| `ServerError::Unavailable(_)` | 503 | unchanged — see rationale |
| `ServerError::UnknownAgent(_)` | 404 | unchanged |
| `ServerError::BadRequest(_)` | 400 | unchanged |
| `ServerError::Unauthorized(_)` | 401/403 | unchanged |

`Unavailable` is a 5xx but is **not** redacted. This crate is its only producer, every message it
emits is operational rather than internal (`"in-flight run limit reached"`), and a 503 that says
`"internal error"` is actively misleading to an operator. The 4xx variants stay detailed because
they describe what the caller sent, which the caller already knows.

The `error` field name and the `ErrorBody` shape are unchanged. No correlation id is added to the
body: keeping `ErrorBody { error: String }` as-is keeps the conformance byte-comparison trivial,
and the `tracing` event below carries `agent` and `run_id` while the run endpoints already return
an `x-run-id` response header.

### Implementation

Each runtime has exactly one choke point:

- axum — `impl IntoResponse for ServerError` in `crates/paigasus-helikon-runtime-axum/src/error.rs`
- actix — `impl ResponseError for ServerError` (`error_response`) in
  `crates/paigasus-helikon-runtime-actix/src/error.rs`

Both branch on the variant, emit `tracing::error!(error = %self, "…")` for the two redacted
variants, and substitute the fixed public string. `status_code` / the status match is unchanged.

The public string is a crate constant so the two runtimes cannot drift:

```rust
/// Body text returned for every HTTP 500. Deliberately non-diagnostic; the
/// underlying error is recorded via `tracing` at `error` level instead.
const PUBLIC_INTERNAL_ERROR: &str = "internal error";
```

### The stream paths

The same runner text escapes through a second channel that the ticket does not mention.
`RunHandle::synthetic_terminal_frame` copies `start_error` into an `AgentEvent::RunFailed { error }`
frame delivered over SSE and WebSocket — a **200** response. Redacting only the 500 body would
leave the disclosure reachable by appending `?stream=sse`, making the fix incomplete.

Both synthetic messages become fixed strings:

| Case | Frame `error` value |
|---|---|
| `start_error` was set (run failed to launch) | `"run failed to start"` |
| no `start_error` (stream ended without a terminal event) | `"run ended before producing a terminal event"` — unchanged, already generic |

`AgentEvent::RunFailed` events produced **by the agent itself** are untouched. That text is the
agent's own contract with its client, not infrastructure detail.

### Where the detail is logged

Not inside `synthetic_terminal_frame`. That method runs once **per subscriber**, so a run watched
by both an SSE client and a WebSocket client would log the same detail twice, and a run with no
subscriber would log it zero times.

Instead the detail is logged once at its source: the `Err(e)` branch of `spawn_writer` in
`handlers/runs.rs`, where `start_error` is assigned.

```rust
Err(e) => {
    tracing::error!(agent = %handle.agent_name, %run_id, error = %e,
                    "run failed to start");
    *handle.start_error.lock().expect("start_error mutex poisoned") = Some(e.to_string());
}
```

`start_error` still stores the detailed text — it is server-side state, and other server-side
consumers may want it. Only the *frame* built from it is redacted. The existing
`synthetic_terminal_frame_branches` unit test in both `registry.rs` files asserts the frame
carries the raw `"boom"` text and must be updated to assert the fixed string instead.

The existing `tracing::warn!` inside `synthetic_terminal_frame` stays (it is a useful per-subscriber
signal) but drops its `%error` field, since the detail is now logged once upstream.

---

## 2. Bind `X-Session-Id` to the authenticated principal

### Public API

Two new public types per runtime, and one breaking trait change.

```rust
/// A stable identity for the authenticated caller, established by the `AuthLayer`.
///
/// Insert into the request's extensions from `AuthLayer::authenticate` to scope
/// every session this caller reaches to that caller alone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Principal(pub String);

/// The compound identity a session is resolved under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionKey<'a> {
    /// The authenticated principal, when one was established.
    pub principal: Option<&'a str>,
    /// The caller-supplied `X-Session-Id`, when present.
    pub id: Option<&'a str>,
}

// BREAKING — was `session(&self, id: Option<&str>)`
#[async_trait]
pub trait SessionProvider: Send + Sync {
    async fn session(&self, key: SessionKey<'_>) -> Result<Arc<dyn Session>, ServerError>;
}
```

`SessionKey` is a struct rather than a second positional parameter so that a future third
component (a tenant id, a scope) is an additive field rather than another breaking signature
change.

### `AuthLayer` is unchanged

Both `AuthLayer` signatures stay exactly as they are — axum's `authenticate(&mut Parts)` and
actix's `#[async_trait(?Send)] authenticate(&HttpRequest)`. Implementations opt in by inserting
`Principal` into request extensions, which is already the crate's documented auth→context bridge:

```rust
// axum
parts.extensions.insert(Principal(claims.sub));

// actix
req.extensions_mut().insert(Principal(claims.sub));
```

This was chosen over changing `authenticate`'s return type to
`Result<Option<Principal>, AuthRejection>`. The return-type change is compiler-enforced, which is
genuinely safer, but it breaks *both* public traits and forces an edit on every existing
`AuthLayer` including ones that will return `Ok(None)`. The extension route breaks one trait
instead of two and reuses a mechanism that already exists; the fail-open risk it introduces is
closed at runtime by the fail-closed rule below rather than at compile time.

### Fail-closed behaviour

| `AuthLayer` | `Principal` in extensions | `X-Session-Id` | Behaviour |
|---|---|---|---|
| not configured | — | any | as today: `principal: None`, one shared namespace |
| configured | present | present | session namespaced to the principal |
| configured | present | absent | fresh, unshared, unstored session |
| configured | **absent** | **present** | **403** — see below |
| configured | absent | absent | fresh, unshared session (no cross-caller leak is possible) |

The 403 row is what makes the extension-based handoff safe. An `AuthLayer` that authenticates but
never establishes a principal would otherwise silently retain the original IDOR. Instead the
server rejects:

```rust
ServerError::Unauthorized(AuthRejection {
    status: StatusCode::FORBIDDEN,
    message: "session id requires an authenticated principal".to_owned(),
})
```

`AgentServerBuilder::allow_unbound_sessions()` sets a `bool` that is carried on `AppStateInner`
alongside `auth`, and disables the check — restoring the previous behaviour for deployments that genuinely want one shared session namespace behind an `AuthLayer`
— a single-tenant service, or a shared API key. It is an explicit, documented opt-out rather than
a default.

When **no** `AuthLayer` is configured at all, nothing changes: every caller is anonymous, shares
one namespace, and no 403 is ever produced. This keeps the development-server and single-tenant
experience identical to today.

### Keying: a tuple, never a concatenation

`InMemorySessionProvider` and `SessionLocks` key on an owned tuple:

```rust
type OwnedKey = (Option<String>, String);   // (principal, id)
```

String concatenation is specifically rejected. `format!("{principal}:{id}")` collides:
`principal = "a:b", id = "c"` and `principal = "a", id = "b:c"` produce the identical key
`"a:b:c"`, reintroducing exactly the cross-principal leak this section closes. Both components
are arbitrary attacker-influenced strings — the principal comes from operator code, the id from a
header — so no separator is safe without length-prefixing. A tuple key has no encoding to get
wrong.

`InMemoryInner` becomes:

```rust
struct InMemoryInner {
    map: HashMap<OwnedKey, Arc<dyn Session>>,
    order: VecDeque<OwnedKey>,
}
```

The FIFO eviction, the `max_sessions` bound, and the anonymous-never-stored rule are unchanged.
`SessionKey { id: None, .. }` still short-circuits to a fresh unstored `MemorySession` regardless
of principal.

### `SessionLocks` takes the same key

```rust
pub(crate) fn lock_for(&self, key: SessionKey<'_>) -> Arc<tokio::sync::Mutex<()>>
```

`SessionLocks` is `pub(crate)`, so this is not a public break — but it is **not** optional. If the
lock map kept keying on the bare id while the session map keyed on the compound key, two
principals using the same id would serialise against each other: principal A could stall
principal B's runs by holding a lock on a guessed id, and could time B's traffic through its own
lock-acquisition latency. That is a cross-tenant DoS and a timing oracle. The lock map keys on the
same tuple, and the existing `Arc::strong_count == 1` pruning is unchanged.

### Call-site change

Identical in both runtimes, in `handlers/runs.rs`:

```rust
let session_id: Option<String> = /* X-Session-Id header, unchanged */;
let principal: Option<String> = /* extensions.get::<Principal>().map(|p| p.0.clone()) */;

if state.auth.is_some()
    && principal.is_none()
    && session_id.is_some()
    && !state.allow_unbound_sessions
{
    return Err(/* 403 as above */);
}

let key = SessionKey { principal: principal.as_deref(), id: session_id.as_deref() };
let session = state.sessions.session(key).await?;
let guard = state.locks.lock_for(key).lock_owned().await;
```

`SessionKey` is `Copy`, so the same value feeds both calls without a clone.

---

## 3. Bound in-flight runs

### Public API

```rust
/// Cap the number of simultaneously in-flight (non-terminal) runs.
///
/// Once this many runs are live, further run creation is rejected with
/// `503 Service Unavailable` until a run reaches a terminal state. Default: 1 024.
pub fn max_in_flight(mut self, max: usize) -> Self
```

The default is finite (1 024, matching `max_retained_runs`) rather than unbounded. Every other
bound in this builder already ships a finite default — `max_retained_runs` 1 024, `max_sessions`
4 096, `max_events_per_run` 10 000 — so an unbounded default would be the outlier, and it would
leave CWE-770 open for anyone who does not opt in. The trade-off accepted here is that a
deployment genuinely running more than 1 024 concurrent runs will start seeing 503s after
upgrading; this is called out in the CHANGELOG.

`build()` rejects `max_in_flight == 0` with `ServerError::BadRequest`, mirroring the existing
`max_sessions == 0` guard — a zero cap would reject every run.

### Implementation

`RunRegistry::create` becomes fallible and performs the admission check inside the write lock it
already acquires, so the check and the insert are one critical section with no TOCTOU window:

```rust
pub fn create(&self, agent_name: String, cancel: CancellationToken)
    -> Result<(Uuid, Arc<RunHandle>), ServerError>
{
    let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
    let in_flight = inner.runs.values()
        .filter(|h| h.terminal_at.lock().expect("terminal_at mutex poisoned").is_none())
        .count();
    if in_flight >= self.max_in_flight {
        return Err(ServerError::Unavailable("in-flight run limit reached".to_owned()));
    }
    /* … mint id, build handle, insert … */
}
```

`RunRegistry` is `pub(crate)` in both crates, so changing `create`'s return type is not a public
break. Lock ordering is `inner` → `terminal_at`, matching `note_terminal` and `sweep`, so the
existing deadlock-freedom argument still holds.

A slot is freed when a run becomes terminal — `note_terminal`, which the `TerminalGuard` in
`spawn_writer` already calls on both the normal and the panic-unwind path. No new bookkeeping and
no separate counter that could drift from the map.

Counting on each `create` is O(live + retained). At the default bounds that is at most a few
thousand cheap mutex reads on a path that then spawns a task and performs network I/O; a
maintained counter would be faster but adds a second source of truth to keep in sync with
eviction. If profiling later shows it matters, the counter is a contained follow-up.

### Response

`503 Service Unavailable`, body `{"error":"service unavailable: in-flight run limit reached"}`
(the existing `Unavailable` `Display` prefix, unredacted per §1), plus a `Retry-After: 1` header
so a well-behaved client backs off rather than hot-looping.

The check runs in `create_run` **after** the per-session lock is acquired, at the point where
`registry.create` is called today. This ordering is deliberate: same-session requests already
queue on the lock, so they do not each consume an admission slot while waiting.

---

## Error handling

No new error variants. The three behaviours reuse existing ones:

- redaction — changes the rendering of `Internal` / `RunStart`, not their construction;
- fail-closed principal — `ServerError::Unauthorized` with a 403 `AuthRejection`, which the
  existing status-clamp already permits;
- admission rejection — `ServerError::Unavailable`, already mapped to 503.

`ServerError` is `#[non_exhaustive]` in both crates, so this remains available for future
additions without a further break.

## Testing

### Conformance suite (`tests/runtime-http-conformance`)

The shared fixture set is currently one agent, `echo`, which always succeeds instantly. Neither
new behaviour is reachable with it, so `scripted_agents()` gains two agents:

| Agent | Behaviour | Exercises |
|---|---|---|
| `boom` | `run()` returns `Err(AgentError)` | redacted 500 body; redacted SSE and WS synthetic frames |
| `hang` | returns a stream that never yields a terminal event until cancelled | in-flight cap → deterministic 503 |

Adding agents to the shared set changes the `GET /agents` response on both runtimes
simultaneously, so the existing set-equality assertion continues to hold.

New parity assertions:

1. `POST /agents/boom/runs` → 500 on both, bodies byte-identical, body is exactly
   `{"error":"internal error"}` and contains no substring of the underlying agent error.
2. `POST /agents/boom/runs?stream=sse` → the synthetic `run_failed` frame is byte-identical
   across runtimes and carries `"run failed to start"`.
3. WebSocket subscribe to a `boom` run → same redacted frame on both.
4. With `max_in_flight(1)`: one `hang` run started via `?mode=async`, then a second request →
   503 on both, byte-identical body, `Retry-After` present on both.

Assertion 4 needs a second server pair built with `max_in_flight(1)`; the existing pair keeps the
default. The two requests must carry **distinct** `X-Session-Id` values (or none), otherwise the
per-session lock queues the second request instead of letting it reach the admission check —
which would make the test pass for the wrong reason.

Assertion 3 is the suite's first WebSocket check, so
`tests/runtime-http-conformance/Cargo.toml` gains `tokio-tungstenite` as a dev-dependency. It is
already in `[workspace.dependencies]` and already used by both runtimes' own `tests/ws.rs`, so
this adds no new third-party pin.

### Per-crate tests (mirrored in both runtimes)

Session/principal:

- two `SessionKey`s with the same `id` and different `principal` → sessions are **not**
  `Arc::ptr_eq`, and locks are **not** `Arc::ptr_eq`;
- same `principal` and same `id` → both are `Arc::ptr_eq` (the existing affinity guarantee);
- `id: None` → fresh unstored session for every principal, including `None`;
- the tuple-key non-collision case explicitly: `("a:b", "c")` and `("a", "b:c")` resolve to
  distinct sessions;
- each row of the fail-closed matrix, including the 403;
- `allow_unbound_sessions()` turns that 403 back into a shared session;
- FIFO eviction still respects `max_sessions` with compound keys.

In-flight cap:

- `max_in_flight(N)` admits N concurrent runs and rejects the (N+1)th with `Unavailable`;
- a slot is released after `note_terminal`, and the next `create` succeeds;
- terminal-but-retained runs do **not** consume in-flight slots (the whole point of the fix);
- `build()` rejects `max_in_flight(0)`.

Redaction:

- `Internal` and `RunStart` responses render exactly `{"error":"internal error"}`;
- `Unavailable`, `BadRequest`, `UnknownAgent`, `Unauthorized` bodies are unchanged;
- `synthetic_terminal_frame` returns the fixed string when `start_error` is set, and still returns
  `None` when a real terminal was seen.

Existing tests in `tests/auth.rs`, `tests/runs.rs`, `tests/concurrency.rs`, and `tests/ws.rs` need
updating for the new `session()` signature and the fail-closed rule; the auth suites configure an
`AuthLayer`, so any of them that also send `X-Session-Id` must either insert a `Principal` or call
`allow_unbound_sessions()`.

## Documentation

- `crates/paigasus-helikon-runtime-axum/README.md` and
  `crates/paigasus-helikon-runtime-actix/README.md` — replace the interim "the session id is
  caller-controlled / implementations must combine it with the principal" wording that PR #173
  added with the real mechanism, and document `max_in_flight` and the redacted 500.
- `docs/book/src/concepts/axum-server.md` and `docs/book/src/concepts/runtimes.md` — same.
- Rustdoc on `SessionProvider`, `InMemorySessionProvider`, `AuthLayer`, `Principal`, `SessionKey`,
  `max_in_flight`, and `allow_unbound_sessions`. The workspace `missing_docs` lint is `warn` and
  the docs job runs `-D warnings`, so every new public item needs a `///`.
- Both CHANGELOGs, under a `### Breaking` heading, with a migration note:
  - `SessionProvider::session` now takes `SessionKey<'_>`; a custom provider that ignored the
    principal keeps its old behaviour by reading `key.id` alone, and gains isolation by keying on
    both fields.
  - An `AuthLayer` used with `X-Session-Id` must now insert `Principal`, or the server must be
    built with `allow_unbound_sessions()`.
  - 500 response bodies are no longer diagnostic.
  - In-flight runs are capped at 1 024 by default.

## Verification

The full CI gate, run from the worktree:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
cargo build -p paigasus-helikon-runtime-axum --no-default-features
mdbook build docs/book
```

`cargo test --workspace --all-features` is the exact gate — per-crate runs miss the
cross-runtime conformance suite and can mask feature-unification problems.

## Follow-ups (not this PR)

- `GET /agents/{name}/runs/{id}/events` authorises on run id alone, with no principal check. A
  caller who obtains another principal's run id can read that run's full event stream. Mitigated
  today by UUIDv4 unguessability and by run ids not being enumerable, but it is the same class of
  finding as §2 and should be closed once `Principal` exists.
- `X-Session-Id` has no length or character-set bound. The session map is capped by
  `max_sessions`, so this is a per-request memory concern rather than unbounded growth.
- An optional per-principal sub-cap on in-flight runs, so one noisy tenant cannot exhaust the
  global cap and 503 every other tenant.
- A maintained in-flight counter, if the O(n) scan in `create` ever profiles as significant.

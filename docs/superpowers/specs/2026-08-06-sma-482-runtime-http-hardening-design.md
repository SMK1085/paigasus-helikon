# SMA-482 — HTTP runtime hardening: 5xx redaction, session–principal binding, in-flight run cap

**Date:** 2026-08-06
**Linear:** [SMA-482](https://linear.app/smaschek/issue/SMA-482/runtime-axum-runtime-actix-harden-5xx-redaction-session-principal)
**Branch:** `feature/sma-482-runtime-axum-runtime-actix-harden-5xx-redaction-session`
**Crates:** `paigasus-helikon-runtime-axum`, `paigasus-helikon-runtime-actix`, `paigasus-helikon-runtime-http-conformance` (internal)

**Revision 3** — incorporates the adversarial spec review (see "Review changelog" at the end), and
pulls the WebSocket-events authorisation gap (§4) in from the follow-up list at Sven's direction.

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

A fourth item, **CWE-639 again on the WebSocket events endpoint**, was surfaced by the adversarial
review of this spec and pulled into scope at Sven's direction:

4. `GET /agents/{name}/runs/{id}/events` authorises on the run id alone
   (`handlers/events.rs:58-62`), so a caller who obtains another principal's run id can read that
   run's entire event stream. It is the same threat class as item 2, and closing it is only
   tractable *because* item 2 introduces `Principal` — which is why it belongs in this change
   rather than a later one.

All four apply identically to both HTTP runtimes. They share one constraint: **any fix must land
in both runtimes in the same change**, or it silently breaks the wire/API parity that
`tests/runtime-http-conformance` exists to assert.

## Goals

- Close all four findings in `runtime-axum` and `runtime-actix` with no behavioural divergence.
- Keep the redacted detail — route it to `tracing` rather than dropping it.
- Extend the conformance suite so the new behaviours are *asserted* across runtimes, not assumed.
- Carry the breaking API change with a `BREAKING CHANGE:` footer and a migration guide.
- Do not trade a memory-growth bug for an availability bug: the run cap ships with reclamation.

## Non-goals

Explicitly out of scope; each is recorded in "Follow-ups".

- A per-principal sub-cap on in-flight runs, and a per-principal session-eviction bound.
- Any change to `paigasus-helikon-core`.

## Architecture

Four independent changes, applied symmetrically to both runtimes. The two crates have
structurally identical call sites — `handlers/runs.rs` resolves the session, acquires the
per-session lock, then calls `registry.create` — so each change is the same edit twice, modulo
each framework's request type.

No `paigasus-helikon-core` change is involved, so the same-PR core-bump caveat in `CLAUDE.md`
does not apply. Both runtime crates are already released, so release-plz performs the version
bumps through its normal flow. **Because release-plz bumps an additive `feat` on a 0.x crate as a
*patch*, and only a breaking change as a *minor*, the breaking-ness must be declared explicitly**:
the implementation commits use `feat(runtime-axum)!:` / `feat(runtime-actix)!:` with a
`BREAKING CHANGE:` footer. Without that marker the crates would publish as `0.1.6` / `0.1.1` with
a silently incompatible trait. Target versions: axum `0.1.5` → `0.2.0`, actix `0.1.0` → `0.2.0`;
`dependencies_update` then cascades the facade.

The PR title must use a scope that already exists in `.versionrc` on `main` (`pr-title.yml` runs
on `pull_request_target`, so the allowlist is read from the base branch). `runtime`,
`runtime-axum`, and `runtime-actix` are all present, so `feat(runtime)!: SMA-482 …` is safe.

---

## 1. Redact internal detail from 5xx response bodies

### Rule

**Every 5xx renders a fixed public string. No 4xx is redacted.**

| Variant | Status | Body |
|---|---|---|
| `ServerError::Internal(_)` | 500 | `{"error":"internal error"}` |
| `ServerError::RunStart(_)` | 500 | `{"error":"internal error"}` |
| `ServerError::Unavailable(_)` | 503 | `{"error":"service unavailable"}` |
| `ServerError::UnknownAgent(_)` | 404 | unchanged |
| `ServerError::BadRequest(_)` | 400 | unchanged |
| `ServerError::Unauthorized(_)` | 401/403 | unchanged |

Revision 1 exempted `Unavailable` on the grounds that "this crate is its only producer". That
premise is wrong. `ServerError` is public (`src/lib.rs:10`) and both `SessionProvider::session`
(`session.rs:53`) and `ContextProvider::build` (`context.rs:93-98`) return it — and `context.rs`
explicitly instructs operators to use "any other `ServerError` variant for unexpected failures".
A Postgres or Redis session provider returning `Unavailable("pool exhausted: postgres://user:pw@host")`
would render that verbatim into a 503 body. (Today the crate has *zero* `Unavailable` producers,
so the premise was vacuous as well as wrong.) Redacting all three 5xx variants removes the
exception that had to be argued for, and gives an operator a rule that can be audited by reading
one match arm.

The 4xx variants stay detailed because they describe what the caller sent, which the caller
already knows.

`ErrorBody { error: String }` is unchanged and no correlation id is added to the body: keeping the
shape as-is keeps the conformance byte-comparison trivial, the `tracing` events below carry
`agent` and `run_id`, and the run endpoints already return an `x-run-id` response header.

### Implementation

Each runtime has exactly one choke point:

- axum — `impl IntoResponse for ServerError` in `crates/paigasus-helikon-runtime-axum/src/error.rs`
- actix — `impl ResponseError for ServerError` (`error_response`) in
  `crates/paigasus-helikon-runtime-actix/src/error.rs`

Both branch on the variant, emit `tracing::error!(error = %self, "…")` for the redacted variants,
and substitute the fixed public string. The status match is unchanged.

The public strings are crate constants so the two runtimes cannot drift:

```rust
/// Body text for every HTTP 500. Deliberately non-diagnostic; the underlying
/// error is recorded via `tracing` at `error` level instead.
const PUBLIC_INTERNAL_ERROR: &str = "internal error";
/// Body text for every HTTP 503.
const PUBLIC_UNAVAILABLE: &str = "service unavailable";
```

`Retry-After: 1` is set at this same choke point, for `Unavailable` only, so the two runtimes
cannot drift on it and every 503 the crate emits carries it. A fixed one-second value can
synchronise retries into a herd at scale; that is accepted for now and noted as a follow-up, since
jitter belongs to a client backoff policy more than to a server hint.

### The stream paths

The same runner text escapes through a second channel that the ticket does not mention.
`RunHandle::synthetic_terminal_frame` (`registry.rs:43-59`) copies `start_error` into an
`AgentEvent::RunFailed { error }` frame delivered over SSE and WebSocket — a **200** response.
Redacting only the 500 body would leave the disclosure reachable by appending `?stream=sse`.

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

`start_error` still stores the detailed text — it is server-side state and other server-side
consumers may want it. Only the *frame* built from it is redacted. The existing
`synthetic_terminal_frame_branches` unit test in both `registry.rs` files asserts the frame carries
the raw `"boom"` text and must be updated to assert the fixed string instead.

The existing `tracing::warn!` inside `synthetic_terminal_frame` stays (a useful per-subscriber
signal) but drops its `%error` field.

### One reclassification

`crates/paigasus-helikon-runtime-actix/src/handlers/events.rs:72` maps a failed `actix_ws::handle`
— i.e. a malformed WebSocket upgrade request — to `ServerError::Internal`. Under the new rule
every such request emits a `tracing::error!`, so any admitted caller could drive unbounded
error-level log output. A malformed upgrade is a client error: this site is reclassified to
`ServerError::BadRequest`. axum has no equivalent, because its `WebSocketUpgrade` extractor
rejects with its own response before the handler runs.

---

## 2. Bind `X-Session-Id` to the authenticated principal

### Public API

```rust
/// A stable identity for the authenticated caller, established by the `AuthLayer`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Principal(pub String);

/// The compound identity a session is resolved under.
///
/// A provider that keys on `id` alone remains vulnerable to CWE-639. Use
/// [`SessionKey::storage_key`] unless you have a specific reason not to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SessionKey<'a> {
    /// The authenticated principal, when one was established.
    pub principal: Option<&'a str>,
    /// The caller-supplied `X-Session-Id`, when present.
    pub id: Option<&'a str>,
}

impl<'a> SessionKey<'a> {
    /// Construct a key. Required because the struct is `#[non_exhaustive]`.
    pub fn new(principal: Option<&'a str>, id: Option<&'a str>) -> Self { /* … */ }

    /// A collision-free single-string key for backends that need one
    /// (Postgres, Redis, a filesystem path). `None` for an anonymous request,
    /// which must not be stored at all.
    ///
    /// `Some(p)` renders as `p<len>:<principal>:<id>` — the length prefix is
    /// what makes it unambiguous — and `None` renders as `a:<id>` (`a` for
    /// *absent*). The tag is what keeps an absent principal apart from every
    /// `Some` form, `Some("")` included: folding `None` into `""` before the
    /// length prefix would render `(None, "s1")` and `(Some(""), "s1")` — two
    /// genuinely different callers — onto the same string.
    pub fn storage_key(&self) -> Option<String> {
        let id = self.id?;
        Some(match self.principal {
            None => format!("a:{id}"),
            Some(principal) => format!("p{}:{}:{}", principal.len(), principal, id),
        })
    }
}

// BREAKING — was `session(&self, id: Option<&str>)`
#[async_trait]
pub trait SessionProvider: Send + Sync {
    async fn session(&self, key: SessionKey<'_>) -> Result<Arc<dyn Session>, ServerError>;
}
```

`#[non_exhaustive]` plus `new()` is what makes the "additive field" claim true — with public
fields and no such attribute, adding a third component would break every struct literal and
exhaustive match, i.e. exactly as breaking as the signature change the struct exists to avoid.

`storage_key()` exists because the migration path is the security boundary. `SessionProvider` is
public and the current rustdoc (`session.rs:39-49`) actively tells operators to implement their
own for multi-tenancy — custom providers are the *expected* multi-tenant path. A provider that
recompiles against the new signature and reads `key.id` alone stays fully vulnerable while the
CHANGELOG announces the IDOR as fixed, and the 403 below does **not** save it (that fires only
when the principal is *absent*). So `storage_key()` is documented as *the* migration, the
rustdoc on `SessionKey` and on `SessionProvider::session` states plainly that reading `key.id`
alone leaves the provider vulnerable to CWE-639, and the same warning goes in both READMEs.

### `AuthLayer` is unchanged

Both `AuthLayer` signatures stay as they are — axum's `authenticate(&mut Parts)` and actix's
`#[async_trait(?Send)] authenticate(&HttpRequest)`. Implementations opt in by inserting
`Principal` into request extensions, which is already the crate's documented auth→context bridge.

This was chosen over changing `authenticate`'s return type to
`Result<Option<Principal>, AuthRejection>`. The return-type change is compiler-enforced, which is
genuinely safer, but it breaks *both* public traits and forces an edit on every existing
`AuthLayer` including ones that will return `Ok(None)`. The extension route breaks one trait
instead of two and reuses a mechanism that already exists; the fail-open risk it introduces is
closed at runtime by the rule below rather than at compile time.

`Principal` is defined per crate rather than in `paigasus-helikon-core`. A shared type would not
buy a shared `AuthLayer` implementation, because the two `AuthLayer` traits already have
different signatures and different `Send` bounds — so the operator writes two impls either way —
and hoisting it into core would drag core into this PR's release train for no gain.

### Fail-closed behaviour

The check is governed by one resolved boolean, `require_principal`, carried on `AppStateInner`:

```rust
// AgentServerBuilder — one field, two setters, last call wins.
require_principal: Option<bool>,          // None = "decide at build()"

pub fn require_principal(mut self, yes: bool) -> Self { /* Some(yes) */ }
pub fn allow_unbound_sessions(mut self) -> Self { /* Some(false) */ }

// build():
let require_principal = self.require_principal.unwrap_or(self.auth.is_some());
```

Defaulting to `auth.is_some()` keeps the no-auth development server exactly as it is today, while
`require_principal(true)` covers the deployment revision 1 missed: both crates support **embedding**
into a host application that supplies its own authentication — `AgentServer::router()` returns a
plain `Router` (`server.rs:293-320`) and actix's `configure()` returns a `ServiceConfig` closure
(`server.rs:292-331`, and `examples/actix_embed.rs`). In that topology `state.auth` is `None`, so a
rule anchored only on `auth.is_some()` would silently skip the most likely production shape — the
very one the current rustdoc recommends ("a deployment behind an authenticating proxy that already
isolates tenants", `session.rs:71-73`). Such an embedder inserts `Principal` from its own
middleware and sets `require_principal(true)`.

| `require_principal` | `Principal` | `X-Session-Id` | Behaviour |
|---|---|---|---|
| false | present | any | session namespaced to the principal (the key stays compound; no 403) |
| false | absent | any | `principal: None`, one shared namespace (today's behaviour; no 403) |
| true | present | present | session namespaced to the principal |
| true | present | absent | fresh, unshared, unstored session |
| true | **absent** | **present** | **403** |
| true | absent | absent | fresh, unshared session (no cross-caller leak is possible) |

**What the flag does and does not do.** `allow_unbound_sessions()` / `require_principal(false)`
suppresses the 403 **and nothing else**. The key stays compound: a caller who *does* carry a
`Principal` is still isolated to it even when the flag is off. The flag only decides what happens
to a caller with no principal — they fall into the shared `principal: None` namespace instead of
being rejected. Revision 1 described this two different ways in two places; this is the single
definition.

The rejection is:

```rust
ServerError::Unauthorized(AuthRejection {
    status: StatusCode::FORBIDDEN,
    message: "session id requires an authenticated principal".to_owned(),
})
```

Note the rendered body. `ServerError`'s `#[error("unauthorized: {0}")]` wraps `AuthRejection`'s
own `Display` (`error.rs:76-79`, `"{message} ({status})"`), so the wire body is exactly:

```json
{"error":"unauthorized: session id requires an authenticated principal (403 Forbidden)"}
```

That is verbose, but it is the shape every other auth rejection in the crate already produces;
changing `AuthRejection`'s `Display` would alter every existing 401/403 body and is out of scope.
The conformance suite pins this exact string so the two runtimes cannot drift.

**Non-UTF-8 header.** `parts.headers.get("x-session-id").and_then(|v| v.to_str().ok())`
(`handlers/runs.rs:164-168`) silently yields `None` for a non-ASCII value, which would make
`session_id.is_some()` false and skip the 403 — an implicit sixth row where the caller gets a fresh
anonymous session instead of an error. A present-but-non-UTF-8 `X-Session-Id` is now an explicit
`ServerError::BadRequest` (400).

### Keying: a tuple, never a concatenation

`InMemorySessionProvider` and `SessionLocks` key on an owned tuple:

```rust
type OwnedKey = (Option<String>, String);   // (principal, id)
```

String concatenation is specifically rejected for the *internal* key. `format!("{principal}:{id}")`
collides: `principal = "a:b", id = "c"` and `principal = "a", id = "b:c"` produce the identical key
`"a:b:c"`, reintroducing exactly the cross-principal leak this section closes. Both components are
arbitrary attacker-influenced strings — the principal comes from operator code, the id from a
header — so no separator is safe without length-prefixing. A tuple key has no encoding to get
wrong. (`SessionKey::storage_key()` is the length-prefixed form, provided for third-party backends
whose storage genuinely needs a single string.)

`InMemoryInner` becomes:

```rust
struct InMemoryInner {
    map: HashMap<OwnedKey, Arc<dyn Session>>,
    order: VecDeque<OwnedKey>,
}
```

The FIFO eviction and the `max_sessions` bound are unchanged, and
`SessionKey { id: None, .. }` still short-circuits to a fresh unstored `MemorySession` regardless
of principal.

**Known limitation, documented not fixed.** `max_sessions` stays a single global FIFO, so one
principal that creates 4 096 distinct ids evicts every other principal's session
(`session.rs:147-151`), silently resetting their conversations. This is a cross-tenant
data-destruction primitive — the same class of argument used below to justify keying the *lock* map
on the compound key. It is not a disclosure and it does not undermine the IDOR fix, but it does
limit how far the "safe for multi-tenant use" claim can be pushed. The `InMemorySessionProvider`
rustdoc says so explicitly, and a per-principal session bound is filed as a follow-up alongside the
per-principal run cap.

### `SessionLocks` takes the same key

```rust
pub(crate) fn lock_for(&self, key: SessionKey<'_>) -> Arc<tokio::sync::Mutex<()>>
```

`SessionLocks` is `pub(crate)` (`session.rs:171`), so this is not a public break — but it is **not**
optional. If the lock map kept keying on the bare id while the session map keyed on the compound
key, two principals using the same id would serialise against each other: principal A could stall
principal B's runs by holding a lock on a guessed id, and could time B's traffic through its own
lock-acquisition latency. That is a cross-tenant DoS and a timing oracle. The lock map keys on the
same tuple, and the existing `Arc::strong_count == 1` pruning is unchanged.

### Call-site change

In axum (`handlers/runs.rs`):

```rust
let session_id: Option<String> = /* X-Session-Id header; 400 on non-UTF-8 */;
let principal: Option<String> = parts.extensions.get::<Principal>().map(|p| p.0.clone());

if state.require_principal && principal.is_none() && session_id.is_some() {
    return Err(/* 403 as above */);
}

let key = SessionKey::new(principal.as_deref(), session_id.as_deref());
let session = state.sessions.session(key).await?;
let guard = state.locks.lock_for(key).lock_owned().await;
```

`SessionKey` is `Copy`, so the same value feeds both calls without a clone.

**actix differs in one load-bearing way.** `req.extensions()` returns a `Ref<'_, Extensions>`, and
actix handler futures carry no `Send` bound — so holding that `Ref` across the following
`.await` **compiles** and then panics with `already mutably borrowed` the first time a
`ContextProvider` or `AuthLayer` calls `extensions_mut()`. The crate already warns about exactly
this at `auth.rs:38-42`. The actix binding must therefore be explicitly scoped:

```rust
// The `Ref` must be dropped before any `.await`.
let principal: Option<String> = {
    req.extensions().get::<Principal>().map(|p| p.0.clone())
};
```

A per-crate actix test whose `ContextProvider` calls `extensions_mut()` guards this, because the
conformance suite's fixture provider would not trigger it.

---

## 3. Bound in-flight runs

### Public API

```rust
/// Cap the number of simultaneously in-flight (non-terminal) runs.
///
/// Once this many runs are live, further run creation is rejected with
/// `503 Service Unavailable` until a run reaches a terminal state. Default: 1 024.
pub fn max_in_flight(mut self, max: usize) -> Self

/// Maximum wall-clock lifetime of a single run. A run still live after this
/// long is cancelled and marked terminal by the registry sweeper, releasing its
/// in-flight slot. Default: 1 hour.
pub fn max_run_duration(mut self, duration: Duration) -> Self
```

The `max_in_flight` default is finite (1 024, matching `max_retained_runs`) rather than unbounded.
Every other bound in this builder already ships a finite default — `max_retained_runs` 1 024,
`max_sessions` 4 096, `max_events_per_run` 10 000 — so an unbounded default would be the outlier
and would leave CWE-770 open for anyone who does not opt in. A deployment genuinely running more
than 1 024 concurrent runs will start seeing 503s after upgrading; this is called out in the
migration note.

`build()` rejects `max_in_flight == 0` with `ServerError::BadRequest` — unconditionally. (The
existing `max_sessions == 0` guard at `server.rs:204` is conditional on `self.sessions.is_none()`
because a custom provider makes that field moot; `max_in_flight` has no such escape, so it is
*not* a mirror of that guard.)

### Why the cap needs `max_run_duration` to ship with it

A cap without reclamation converts a memory-growth bug into a permanent, unrecoverable outage:

- `RunRegistry::sweep` never evicts non-terminal runs (`registry.rs:163`, and the `retain` at
  `173-193` keeps `terminal_at == None`).
- `?mode=async` deliberately attaches no cancel `DropGuard` (`handlers/runs.rs:34-36`, `201-203`),
  so a client disconnect does not end the run.
- `RunConfig::default().timeout` is `None` (`crates/paigasus-helikon-core/src/runner.rs:188`), so
  by default there is no deadline either.

Together those mean a run that never terminates holds its slot forever. 1 024 cheap
`POST …?mode=async` requests against any slow or hanging agent would permanently 503 the whole
server for every caller until restart — strictly worse than today's "memory grows", and with no
signal and no recovery path. The `hang` conformance agent below is a working proof.

So `sweep` gains a **pass 0**, running before the existing TTL and count-cap passes and under the
same write lock:

> For each run with `terminal_at == None` and `created_at + max_run_duration <= now`: call
> `handle.cancel.cancel()`, stamp `terminal_at = now`, push the id onto `completion_order`, and
> decrement the live counter.

Cancelling drives the writer task to finish, whose `TerminalGuard` calls `note_terminal` — which
is already idempotent, so the double-stamp is a no-op. `RunHandle` gains a `created_at: Instant`
set in `create`. The lock order stays `inner` → `terminal_at`, matching `note_terminal` and the
existing passes, so the deadlock-freedom argument is unchanged.

One hour is deliberately generous: it is long enough not to interrupt legitimate long-running
agents, and finite enough that a wedged run self-heals rather than permanently consuming capacity.

### Implementation

`RunRegistry::create` becomes fallible. `RegistryInner` gains a maintained counter rather than a
scan:

```rust
struct RegistryInner {
    runs: HashMap<Uuid, Arc<RunHandle>>,
    completion_order: VecDeque<Uuid>,
    /// Count of entries in `runs` whose `terminal_at` is `None`.
    live: usize,
}
```

`live` is mutated at exactly three sites, all of which already hold `inner.write()`: `create`
(+1), `note_terminal` (−1, only when it actually stamps), and `sweep` pass 0 (−1, same condition).
Eviction of terminal runs does not touch it. Because every mutation is inside the one write lock
and `sweep` never removes a non-terminal run, the counter cannot drift from the map.

Revision 1 proposed recomputing the count by scanning `inner.runs` on every `create` and rejected
a counter as "a second source of truth". That was the wrong call: the scan holds the write lock
while taking up to `max_runs + max_in_flight` (~2 048 at defaults) `std::sync::Mutex` locks,
serialising against every concurrent `get` from the WebSocket endpoint and every `note_terminal`,
and the counter provably cannot drift.

```rust
pub fn create(&self, agent_name: String, principal: Option<String>, cancel: CancellationToken)
    -> Result<(Uuid, Arc<RunHandle>), ServerError>
{
    let mut inner = self.inner.write().expect("RunRegistry RwLock poisoned");
    if inner.live >= self.max_in_flight {
        tracing::warn!(live = inner.live, cap = self.max_in_flight,
                       "rejecting run: in-flight limit reached");
        return Err(ServerError::Unavailable("in-flight run limit reached".to_owned()));
    }
    /* … mint id, build handle with created_at and principal, insert, inner.live += 1 … */
}
```

The `warn!` is what lets an operator see the cliff coming; without it the cap's only signal is a
503 the caller sees and the server does not record.

`RunRegistry` is `pub(crate)` in both crates, so changing `create`'s return type is not a public
break.

### Response

`503 Service Unavailable`, body `{"error":"service unavailable"}` per §1, with `Retry-After: 1`.
The specific reason (`"in-flight run limit reached"`) goes to `tracing`, not the wire — it would
otherwise confirm to an attacker that their resource-exhaustion attempt is working and that the
cap is finite.

The check runs where `registry.create` is called today, i.e. **after** the per-session lock is
acquired. This ordering is deliberate: same-session requests already queue on the lock, so they do
not each consume an admission slot while waiting.

**Slot-leak audit.** A slot is consumed only by a successful `create` and released by
`note_terminal` or `sweep` pass 0. `create` is the *last* fallible step before `spawn_writer`
(`handlers/runs.rs:185-198`) — the agent lookup, the body parse, the session resolution, the 403
check, and the context build all happen before it, so no error path can consume a slot and return
early. After `create`, `spawn_writer`'s `TerminalGuard` calls `note_terminal` on both the normal
and the panic-unwind path (`handlers/runs.rs:259-264`). A client disconnect either cancels the run
(one-shot / SSE `DropGuard`) or leaves it running to completion (`?mode=async`); the pathological
"never completes" case is what pass 0 covers.

---

## 4. Authorise the WebSocket events endpoint against the principal

`handlers/events.rs` currently authorises a subscription on two facts: the run id exists, and its
`agent_name` matches the path segment (`events.rs:58-62`). Nothing ties the run to a caller, so any
admitted caller holding another principal's run id can replay and live-tail that run's entire event
stream — every message, tool call, and result. Run ids are UUIDv4 and are not enumerable, which is
why this was survivable; it is still the same IDOR class as §2, and §2's `Principal` is what makes
the fix a few lines rather than a redesign.

### Mechanism

`RunHandle` gains an owning principal, captured at creation:

```rust
pub(crate) struct RunHandle {
    pub agent_name: String,
    /// Principal that started this run; `None` for an unbound run.
    pub principal: Option<String>,
    /* … */
}

// pub(crate), so not a public break
pub fn create(&self, agent_name: String, principal: Option<String>,
              cancel: CancellationToken) -> Result<(Uuid, Arc<RunHandle>), ServerError>
```

`create_run` passes the principal it already resolved for §2. The events handler resolves the
requesting principal the same way and compares:

```rust
let handle = state
    .registry
    .get(run_id)
    .filter(|h| h.agent_name == name)
    .filter(|h| h.principal.as_deref() == principal.as_deref())   // NEW
    .ok_or_else(|| ServerError::UnknownAgent(format!("{name}/{id}")))?;
```

### Why 404 and not 403

The mismatch is folded into the **existing 404**, deliberately. A distinct 403 would confirm that
the run id exists and belongs to someone else, turning the endpoint into an existence oracle and
handing an attacker a way to validate harvested ids. A 404 is indistinguishable from "no such run",
which is the property worth having. This also means the change adds no new status code to the
endpoint and no new OpenAPI response — only the 404 description widens.

### Compatibility

The comparison is `Option<String>` equality, so when no principal is ever established — no
`AuthLayer`, `require_principal` false — every run has `principal: None`, every request resolves
`None`, and every comparison succeeds. The single-tenant and development-server behaviour is
bit-for-bit unchanged; the gate only bites once principals exist, which is exactly when it should.

One consequence worth stating: a run started by principal A can no longer be observed by principal
B **even if B is an operator**. There is no administrative override, and adding one is out of scope.

### Framework difference

The axum handler's current signature takes `State`, `Path`, and `WebSocketUpgrade` and never sees
the request extensions. It gains `principal: Option<Extension<Principal>>` (`Principal` is `Clone`,
and the extractor ordering is unaffected because `WebSocketUpgrade` stays last). actix reads
`req.extensions()` inside an explicit scope, under the same `RefCell` rule as §2 — the `Ref` must
be dropped before the upgrade `.await`.

---

## Error handling

No new error variants. The three behaviours reuse existing ones:

- redaction — changes the rendering of `Internal` / `RunStart` / `Unavailable`, not their construction;
- fail-closed principal — `ServerError::Unauthorized` with a 403 `AuthRejection`, which the
  existing status-clamp already permits;
- admission rejection — `ServerError::Unavailable`, already mapped to 503;
- malformed `X-Session-Id`, and actix's malformed WS upgrade — `ServerError::BadRequest`.

`ServerError` is `#[non_exhaustive]` in both crates, so this remains available for future
additions without a further break.

## OpenAPI

`handlers/openapi.rs` in both crates enumerates the documented responses for each route
(`openapi.rs:51-61`) and currently lists 200/202/400/401/403/404/500 for `POST /agents/{name}/runs`
— no 503. Shipping a cap whose failure mode is undocumented breaks client codegen, and the parity
suite would not catch it because it only asserts *path* keys (`parity.rs:327-337`). Both files
gain:

```rust
(status = 503, description = "In-flight run limit reached; retry after the `Retry-After` interval"),
```

and the 403 description is extended to cover the missing-principal case. For the events route
(`openapi.rs:76-78`) the **404 description** widens to cover a run owned by a different principal —
per §4 that case deliberately reuses 404 rather than adding a status code. The conformance suite
gains a response-set parity assertion so this class of drift is caught next time.

## Testing

### Conformance suite (`tests/runtime-http-conformance`)

`ScriptedAgent` (`src/lib.rs:24-50`) always returns `Ok(stream::iter(…))` over a finite
`Vec<AgentEvent>` and can express neither new fixture, so it gains a behaviour discriminant:

```rust
enum Behaviour {
    Script(Vec<AgentEvent>),   // today's agents
    FailToStart,               // run() -> Err(AgentError)
    Hang,                      // run() -> Ok(stream::pending().boxed())
}
```

| Agent | Behaviour | Exercises |
|---|---|---|
| `echo` | `Script` | existing assertions, unchanged |
| `boom` | `FailToStart` | redacted 500 body; redacted SSE and WS synthetic frames |
| `hang` | `Hang` | in-flight cap → deterministic 503 |

`futures::stream::pending()` is sufficient for `hang`: `TokioRunner::controlled` already selects on
the cancel token (`crates/paigasus-helikon-runtime-tokio/src/lib.rs:70-88`), so the agent need not
handle cancellation itself.

Adding agents to the shared set changes `GET /agents` on both runtimes simultaneously, so the
existing set-equality assertion continues to hold.

New parity assertions:

1. `POST /agents/boom/runs` → 500 on both, bodies byte-identical, body is exactly
   `{"error":"internal error"}` and contains no substring of the underlying agent error.
2. `POST /agents/boom/runs?stream=sse` → the synthetic `run_failed` frame is byte-identical across
   runtimes and carries `"run failed to start"`.
3. WebSocket subscribe to a `boom` run → same redacted frame on both.
4. With `max_in_flight(1)`: one `hang` run started via `?mode=async`, then a second request → 503
   on both, byte-identical body `{"error":"service unavailable"}`, `Retry-After` present on both.
5. **Fail-closed 403.** With an `AuthLayer` configured that admits the request but inserts no
   `Principal`, `POST` with `X-Session-Id` → 403 on both, bodies byte-identical and equal to the
   exact string pinned in §2.
6. **Principal isolation, end to end.** With an `AuthLayer` that derives `Principal` from a header,
   two requests carrying the same `X-Session-Id` but different principals must not share
   conversation history — asserted identically on both runtimes.
7. **Response-set parity for `/openapi.json`** — the documented status codes for each path match
   between runtimes, not just the path keys.
8. **WebSocket cross-principal denial (§4).** With the `AuthLayer` pair from assertion 6: start a
   run as principal A, then attempt `GET /agents/echo/runs/{id}/events` as principal B. Both
   runtimes must return **404** — not 403, and not an upgrade — with byte-identical bodies. A
   companion sub-assertion confirms principal A *can* subscribe to its own run, so the test cannot
   pass by denying everyone.

Assertions 5 and 6 are the ones the parity suite most needs, because the 403 is the most
security-critical new response *and* the two implementations diverge most there: axum uses
`from_fn_with_state` plus a `Request::from_parts` reassembly (`server.rs:364-378`), actix a
hand-rolled `AuthGuard` short-circuit (`middleware.rs:98-113`).

Assertions 4–6 need additional server pairs (one with `max_in_flight(1)`, one with an `AuthLayer`);
the existing pair keeps the defaults. For assertion 4 the two requests must carry **distinct**
`X-Session-Id` values (or none), otherwise the per-session lock queues the second request instead of
letting it reach the admission check — which would make the test pass for the wrong reason. That
pair is single-purpose: its `hang` run is uncancellable from the test (no public cancel API, no
`DropGuard` on the async path) and holds its slot until `max_run_duration` elapses, so nothing else
should be asserted against it. Assertion 4's sequencing is race-free — the 202 returns only after
`registry.create` — and `boot_actix()` (`parity.rs:51-71`) already spawns a detached, never-shut-down
thread per pair, so the extra pairs follow an established (if untidy) pattern.

Assertion 3 is the suite's first WebSocket check, so `tests/runtime-http-conformance/Cargo.toml`
gains `tokio-tungstenite` as a dev-dependency. It is already in `[workspace.dependencies]` and
already used by both runtimes' own `tests/ws.rs`, so this adds no new third-party pin.

### Per-crate tests (mirrored in both runtimes)

Session/principal:

- two `SessionKey`s with the same `id` and different `principal` → sessions are **not**
  `Arc::ptr_eq`, and locks are **not** `Arc::ptr_eq`. **Both lock `Arc`s must be held
  simultaneously** for the lock half: `lock_for` prunes entries with `Arc::strong_count == 1` on
  every call (`session.rs:203`), so dropping the first before taking the second makes the assertion
  hold even against a buggy bare-id implementation;
- positive control: same principal + same id, both `Arc`s held → `ptr_eq` for session and lock;
- `id: None` → fresh unstored session for every principal, including `None`;
- explicit non-collision: `("a:b", "c")` and `("a", "b:c")` resolve to distinct sessions, and their
  `storage_key()` values differ;
- every row of the fail-closed matrix, including the 403 and its exact body;
- **`allow_unbound_sessions()` with a principal present** still isolates (the row revision 1's
  ambiguity left untested);
- `require_principal(true)` with **no** `AuthLayer` configured (the embedded-host topology) still
  produces the 403;
- non-UTF-8 `X-Session-Id` → 400;
- FIFO eviction still respects `max_sessions` with compound keys;
- actix only: a `ContextProvider` that calls `extensions_mut()` does not panic (the `RefCell`
  borrow guard).

In-flight cap:

- `max_in_flight(N)` admits N concurrent runs and rejects the (N+1)th with `Unavailable`;
- a slot is released after `note_terminal`, and the next `create` succeeds;
- terminal-but-retained runs do **not** consume in-flight slots (the point of the fix);
- `sweep` pass 0 cancels and terminalises a run past `max_run_duration`, and the freed slot admits
  a new run (driven with an injected `Instant`, as the existing sweep tests do);
- `build()` rejects `max_in_flight(0)`.

WebSocket authorisation (§4):

- a run created with `principal: Some("a")` is not reachable by a request resolving
  `Some("b")` or `None` → 404, and the response is byte-identical to a genuinely-unknown run id
  (no oracle);
- the owning principal *can* subscribe (the positive control);
- with no principal anywhere (`None` == `None`), subscription still succeeds — the
  single-tenant path is unchanged;
- the agent-name mismatch check still returns 404 independently of the principal check.

Redaction:

- `Internal` and `RunStart` render exactly `{"error":"internal error"}`; `Unavailable` renders
  exactly `{"error":"service unavailable"}` with `Retry-After`;
- `BadRequest`, `UnknownAgent`, `Unauthorized` bodies are unchanged;
- `synthetic_terminal_frame` returns the fixed string when `start_error` is set, and still returns
  `None` when a real terminal was seen.

Existing tests in `tests/auth.rs`, `tests/runs.rs`, `tests/concurrency.rs`, and `tests/ws.rs` need
updating for the new `session()` signature and the fail-closed rule; the auth suites configure an
`AuthLayer`, so any that also send `X-Session-Id` must either insert a `Principal` or call
`allow_unbound_sessions()`.

## Documentation

- `crates/paigasus-helikon-runtime-axum/README.md:81-91` and
  `crates/paigasus-helikon-runtime-actix/README.md:101-111` — the section headed "Security: the
  session id is caller-controlled" is the interim wording PR #173 added. Replace it with the real
  mechanism, and carry the **long-form migration guide** here (see below). Also document
  `max_in_flight`, `max_run_duration`, and the redacted 5xx.
- `docs/book/src/concepts/axum-server.md` — lines 76-78 (session affinity), the builder table at
  95-96 (add `max_in_flight`, `max_run_duration`), and the `SessionProvider` signature at 104-113.
  `docs/book/src/concepts/runtimes.md` carries no session-security wording and needs an edit only
  if the builder-knob summary there changes.
- Rustdoc on `SessionProvider`, `InMemorySessionProvider`, `AuthLayer`, `Principal`, `SessionKey`,
  `SessionKey::storage_key`, `max_in_flight`, `max_run_duration`, `require_principal`, and
  `allow_unbound_sessions`. The workspace `missing_docs` lint is `warn` and the docs job runs
  `-D warnings`, so every new public item needs a `///`.

**CHANGELOGs are not hand-edited.** Both files are git-cliff output with a bare `## [Unreleased]`
heading (`crates/paigasus-helikon-runtime-axum/CHANGELOG.md:8`); prose placed there is orphaned when
release-plz inserts `## [0.2.0]` beneath it. The breaking notice travels as a `BREAKING CHANGE:`
footer in the commit body — which git-cliff renders into the generated CHANGELOG *and* is what
drives the minor bump on a 0.x crate. The long-form migration guide lives in the crate READMEs and
the mdBook, which are the pages a reader actually lands on from crates.io and docs.rs.

Migration content:

- `SessionProvider::session` now takes `SessionKey<'_>`. Use `key.storage_key()` for a single-string
  backend key. **Reading `key.id` alone preserves the old behaviour *and* the CWE-639
  vulnerability.**
- An `AuthLayer` used with `X-Session-Id` must now insert `Principal`, or the server must be built
  with `allow_unbound_sessions()`.
- Embedded deployments with host-supplied auth should insert `Principal` and set
  `require_principal(true)`.
- 5xx response bodies are no longer diagnostic.
- In-flight runs are capped at 1 024 by default, and a run still live after 1 hour is cancelled.
- A run's WebSocket event stream is readable only by the principal that started it; other
  principals receive 404. There is no administrative override. Deployments with no principals are
  unaffected.

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

`cargo test --workspace --all-features` is the exact gate — per-crate runs miss the cross-runtime
conformance suite and can mask feature-unification problems.

## Follow-ups (not this PR)

- Per-principal sub-cap on in-flight runs, so one noisy tenant cannot exhaust the global cap.
- Per-principal bound on `max_sessions`, closing the cross-tenant session-eviction primitive
  documented in §2.
- Jitter on `Retry-After` to avoid synchronised client retries.

## Review changelog

Folded in from the adversarial review:

| Sev | Finding | Resolution |
|---|---|---|
| BLOCKER | Documented migration (`read key.id alone`) re-opens the IDOR | Added `SessionKey::storage_key()` as *the* migration; rustdoc + README state plainly that `key.id` alone stays vulnerable |
| BLOCKER | `allow_unbound_sessions()` described two incompatible ways | Single definition: it suppresses the 403 only; the key stays compound. Added the missing test row |
| BLOCKER | A wedged run's in-flight slot is never reclaimed → permanent 503 brick | Added `max_run_duration` (default 1 h) and `sweep` pass 0; added `warn!` on rejection |
| MAJOR | 403 anchored on `auth.is_some()` is fail-open for embedded deployments | Added `require_principal(bool)`, defaulting to `auth.is_some()` |
| MAJOR | "`Unavailable` — this crate is its only producer" is false | All 5xx now redacted; rule has no exception |
| MAJOR | No conformance assertion for the 403 | Added assertions 5 and 6 |
| MAJOR | `openapi.rs` not updated; 503 undocumented | Added, plus a response-set parity assertion |
| MAJOR | Hand-edited CHANGELOG conflicts with git-cliff/release-plz | `BREAKING CHANGE:` footer; migration guide in READMEs + book |
| MAJOR | actix `Ref` held across `.await` panics | Separate scoped snippet + a dedicated actix test |
| MAJOR | `max_sessions` global FIFO is a cross-tenant eviction primitive | Documented as a known limitation; follow-up filed |
| MINOR | `SessionKey`'s "additive" rationale false without `#[non_exhaustive]` | Added `#[non_exhaustive]` + `new()` |
| MINOR | `Retry-After` placement unspecified | Set at the error-rendering choke point |
| MINOR | Lock-isolation test can pass vacuously | Both `Arc`s held simultaneously + positive control |
| MINOR | actix malformed WS upgrade → `Internal` → unbounded `error!` | Reclassified to `BadRequest` |
| MINOR | `ScriptedAgent` cannot express `boom`/`hang` | Added a `Behaviour` discriminant; `stream::pending()` for `hang` |
| MINOR | Extra server pair is single-purpose | Stated |
| MINOR | `max_in_flight == 0` guard does not "mirror" `max_sessions` | Corrected to unconditional |
| MINOR | Non-UTF-8 `X-Session-Id` bypasses the 403 | Explicit 400 |
| MINOR | Counter dismissed on a weak premise | Adopted the counter; recorded the real reason (lock-hold cost) |
| QUESTION | What forces a *minor* bump? | `feat(scope)!:` + `BREAKING CHANGE:` footer, stated in Architecture |
| QUESTION | Exposing the 503's reason | Redacted; reason goes to `tracing` |
| QUESTION | Which mdBook pages | Verified: `axum-server.md` only; `runtimes.md` carries no session wording |

Considered and **not** acted on:

- **`Principal` in `paigasus-helikon-core` instead of per crate.** The two `AuthLayer` traits
  already have different signatures and different `Send` bounds, so an operator writes two impls
  either way; a shared type buys nothing and would drag core into this PR's release train.
- **Closing the WebSocket events IDOR in this PR.** Raised as an explicit scope question at the
  approval gate rather than absorbed silently — and **accepted**: it is now §4. The argument that
  won was that it is only cheap *because* §2 lands in the same change, so deferring it would mean
  paying to re-establish the context later.

Also updated the migration content and the `Documentation` section for §4: the crate READMEs and
`docs/book/src/concepts/axum-server.md` must state that a run's event stream is now readable only
by the principal that started it, and that there is no administrative override.

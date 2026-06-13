# SMA-412 — `paigasus-helikon-tools`: WebFetch + WebSearch network tools

**Status:** approved (brainstorming) — pending written-spec review
**Ticket:** [SMA-412](https://linear.app/smaschek/issue/SMA-412/paigasus-helikon-tools-webfetch-websearch-network-tools)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-412-paigasus-helikon-tools-webfetch-websearch-network-tools`
**Date:** 2026-06-13
**Builds on:** [SMA-328 design](./2026-06-13-tools-sandbox-harness-design.md) (the FS/Bash harness; §11 deferred these web tools)

## 1. Summary

Add the two **network** tools to `paigasus-helikon-tools`, deferred from SMA-328
(§11): `WebFetchTool` (HTTP(S) fetch → readability → Markdown) and
`WebSearchTool` (a pluggable `SearchBackend` trait with two real backends, Brave
and Tavily). Both reuse the `Tool` / `ToolError` / builder conventions
established by the FS/Bash tools. Everything lands behind a new, off-by-default
`web` Cargo feature so consumers who only want the FS/Bash subset never pull
`reqwest`.

This is a **plain additive `feat`**: the tools crate is already published at
`0.1.0`, `ToolError::Denied` already exists (added in SMA-328), and no
`paigasus-helikon-core` change is required — so there is **no ascend ritual and
no manual core/facade bump** (contrast SMA-328, which ascended the crate from a
stub and added core API).

## 2. Scope decisions (resolved during brainstorming)

These five decisions were made explicitly and drive the rest of the design.

1. **`WebFetch` returns clean Markdown, not raw HTML.** Fetch → Readability
   main-content extraction → HTML→Markdown conversion. Chosen over raw-body
   return so the output is directly model-useful (closest to Claude Code's
   `WebFetch`). Cost: two new pure-Rust dependencies (a readability extractor +
   a markdown converter).

2. **Two real search backends ship in this PR: Brave *and* Tavily.** Both sit
   behind the `SearchBackend` trait, proving the abstraction is genuinely
   swappable (not just abstractly). The tool holds an `Arc<dyn SearchBackend>`
   so the backend is swapped at runtime.

3. **Pure domain allow/deny, no SSRF guard.** `WebFetch` enforces only an
   optional allow-list / deny-list on the URL host — the same posture as
   `BashTool`'s command allow/deny. Default is permissive (fetch any public
   URL). There is **no** built-in private/loopback/link-local IP blocking;
   per the SMA-328 two-layer model, the runner's `PermissionPolicy`/deny-rules
   is the real gate and the SSRF posture is the operator's responsibility.

4. **Approach A — two independent tools, no shared public primitive.**
   `WebFetchTool` and `WebSearchTool` (and each backend) construct their own
   `reqwest::Client` through a private `web::http::build_client()` helper that
   keeps client config (TLS features, user-agent, redirect policy) DRY. No
   public `WebClient`/`Sandbox`-style primitive is introduced — the consumer
   surface stays as small as the FS-tools family.

5. **Single `web` feature, off by default.** Gates `reqwest` + the HTML deps +
   the whole `web` module. Not split into `web-fetch`/`web-search` (YAGNI; both
   need `reqwest`).

## 3. Integration surface (existing APIs we build against)

Verified against the current tree; **all already exist** — this PR adds no core
API.

- **`Tool<Ctx>` trait** (`core/src/tool.rs:64`) — `#[async_trait]`; `name()`,
  `description()`, `schema() -> &serde_json::Value`, `output_schema()` (default
  `None`), `effect() -> ToolEffect` (default `SideEffect`), and `async fn
  invoke(&self, ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput,
  ToolError>`. Object-safe; agents hold `Arc<dyn Tool<Ctx>>`.
- **`ToolEffect`** (`core/src/tool.rs:19`) — `ReadOnly | Write | SideEffect`.
  The doc comment classifies **network as `SideEffect`** explicitly; both web
  tools use it. Consequence: they are blocked under `Plan` mode (which allows
  only `ReadOnly`) — accepted, following core's documented taxonomy.
- **`ToolOutput`** (`core/src/tool.rs:235`) — `{ content: serde_json::Value }`,
  via `ToolOutput::new(content)`.
- **`ToolError`** (`core/src/tool.rs:253`) — `InvalidArgs { schema_errors }`
  (recoverable per ADR-10), `Denied { reason }` (added SMA-328), and
  `Other(#[from] anyhow::Error)`. **We add no variant.**
- **Phantom-`Ctx` pattern** — tools that ignore the user context carry
  `PhantomData<fn() -> Ctx>` so one value serves agents of any `Ctx`
  (`BashTool`, `McpTool` precedent).
- **Conventions** — `#[async_trait]` on async traits; builder structs for
  configurable tools (`BashToolBuilder` precedent); input types derive
  `serde::Deserialize + schemars::JsonSchema`; the JSON schema is generated once
  at construction and stored as a `serde_json::Value` so `schema()` returns a
  borrow; `#[non_exhaustive]` on public enums/structs.

## 4. Crate layout

New code under a gated `web/` module; existing FS/Bash files unchanged.

```
crates/paigasus-helikon-tools/
  Cargo.toml          # + optional web deps; + [features] web
  src/
    lib.rs            # + #[cfg(feature = "web")] re-exports
    sandbox.rs read.rs write.rs edit.rs bash.rs   # unchanged
    web/
      mod.rs          # gated module root; pub use of the web surface
      http.rs         # private: build_client(), host matching, redirect policy
      fetch.rs        # WebFetchTool + WebFetchToolBuilder
      search.rs       # WebSearchTool + WebSearchToolBuilder + SearchBackend + SearchResult
      backends/
        mod.rs        # pub use brave::BraveBackend, tavily::TavilyBackend
        brave.rs      # BraveBackend
        tavily.rs     # TavilyBackend
  tests/
    web_fetch.rs      # extraction (pure), domain deny (pure), localhost round-trip
    web_search.rs     # ScriptedBackend drives the tool; Brave/Tavily JSON-fixture parse tests
    fixtures/
      brave_search.json
      tavily_search.json
      article.html     # readability/markdown extraction fixture
  examples/
    web_research.rs   # real OpenAiModel + WebSearch + WebFetch (manual, behind API keys)
```

Public re-exports from `lib.rs`, all behind `#[cfg(feature = "web")]` and each
carrying a `///` doc comment (workspace `missing_docs = "warn"` + `-D warnings`):
`WebFetchTool`, `WebFetchToolBuilder`, `WebSearchTool`, `WebSearchToolBuilder`,
`SearchBackend`, `SearchResult`, `BraveBackend`, `TavilyBackend`.

## 5. Feature gating

In `crates/paigasus-helikon-tools/Cargo.toml`:

```toml
[features]
web = ["dep:reqwest", "dep:url", "dep:dom_smoothie", "dep:htmd"]
```

`reqwest`, `url`, `dom_smoothie`, `htmd` are declared `optional = true`.
`serde`/`serde_json`/`schemars`/`async-trait`/`anyhow`/`tokio` are already
non-optional deps of the crate and are reused as-is.

In the facade (`crates/paigasus-helikon/Cargo.toml`), add a feature that
forwards into the crate (kebab-case, per the facade convention):

```toml
tools-web = ["tools", "paigasus-helikon-tools/web"]
```

The existing `tools` feature + `pub use paigasus_helikon_tools as tools`
re-export (`paigasus-helikon/src/lib.rs:35`) already surface the crate; with
`tools-web` enabled the web tools appear under the same `tools` module. The
re-export doc line gains a note that the web tools require `tools-web`.

## 6. `WebFetchTool`

- `name() = "WebFetch"`, `effect() = ToolEffect::SideEffect`.
- No `Sandbox` (web tools are not filesystem-bound). Built via a builder
  mirroring `BashToolBuilder`:

  ```rust
  WebFetchTool::builder()
      .timeout(Duration)         // default 30s (whole-request)
      .max_body_bytes(5 << 20)   // default 5 MiB; cap downloaded body, flag truncation
      .allow_domains(["docs.rs", "example.com"])  // optional; if set, ONLY these (+subdomains)
      .deny_domains(["evil.test"])                // optional; deny ALWAYS wins over allow
      .user_agent("paigasus-helikon/<ver>")       // default UA
      .build::<Ctx>()
  ```

- Input args: `{ url: String }`. **No `prompt` field** — prompt-driven
  summarization needs a `Model` and is out of scope (§12).
- **Flow inside `invoke`:**
  1. Parse `url` with the `url` crate. Scheme not `http`/`https` ⇒
     `Denied { reason: "only http/https URLs are supported" }`.
  2. Extract the host; run the allow/deny check (§6.1). Blocked ⇒
     `Denied { reason }`. **(Satisfies the AC.)**
  3. `GET` via the shared client with a **custom redirect policy that re-runs
     the host check on every hop** — a redirect from an allowed host into a
     blocked host is refused (the request fails and maps to
     `Denied { reason: "redirect to a blocked domain: <host>" }`), so the
     deny-list cannot be bypassed via redirect.
  4. Read the response body incrementally, stopping at `max_body_bytes`; past
     the cap ⇒ truncate the body and set `truncated: true`.
  5. Branch on `Content-Type`:
     - HTML (`text/html`, `application/xhtml+xml`) ⇒ `dom_smoothie` readability
       extraction of the main article subtree → `htmd` HTML→Markdown ⇒
       `format: "markdown"`.
     - Other textual types (`text/plain`, `text/*`, `application/json`,
       `application/xml`, …) ⇒ return the body as lossy UTF-8 unchanged ⇒
       `format: "text"`.
     - Non-text content types ⇒
       `Denied { reason: "unsupported non-text content type: <ct>" }`
       (mirrors `ReadTool`'s deliberate non-UTF-8 refusal).
- **Output:**

  ```json
  {
    "url": "<final URL after redirects>",
    "status": 200,
    "content_type": "text/html; charset=utf-8",
    "content": "<markdown or text>",
    "format": "markdown",
    "truncated": false
  }
  ```

- **Soft outcomes are reported in the output, not raised** (the `BashTool`
  precedent): a non-2xx HTTP status is returned in `status` with best-effort
  extraction of whatever body came back — the model inspects it. Body
  truncation is reported in `truncated`. Only scheme/domain/content-type
  refusals are `Denied`; transport failures (DNS, TLS, connect, timeout) are
  `Other`.

### 6.1 Domain matching semantics

Case-insensitive. A list entry matches the request host if the host **equals**
the entry or is a **sub-domain** of it (host equals `entry` or ends with
`"." + entry`). So `example.com` covers `example.com` and `api.example.com` but
not `notexample.com`. If an allow-list is configured, only matching hosts pass;
a deny-list match always refuses, taking precedence over the allow-list. This is
a hard-safety invariant enforced inside the tool (it is *not* a
`PermissionPolicy` re-invocation — consistent with SMA-328's two-layer model).

## 7. `WebSearchTool` + `SearchBackend`

```rust
/// A swappable search backend. Implement this to add a provider.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Backend name, for diagnostics / user-agent.
    fn name(&self) -> &str;
    /// Run a query, returning at most `limit` normalized results.
    async fn search(&self, query: &str, limit: usize)
        -> Result<Vec<SearchResult>, anyhow::Error>;
}

/// One normalized search hit.
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// Richer page content when the backend supplies it (Tavily); `None` for Brave.
    pub content: Option<String>,
}
```

- `WebSearchTool` holds an **`Arc<dyn SearchBackend>`** — a single concrete tool
  type whose backend is swapped at runtime (directly satisfies "swappable via
  the trait"). `name() = "WebSearch"`, `effect() = ToolEffect::SideEffect`.

  ```rust
  let backend = Arc::new(BraveBackend::from_env()?);   // or TavilyBackend
  WebSearchTool::builder(backend).max_results(5).build::<Ctx>();
  ```

- Input args: `{ query: String, limit: Option<usize> }`. `limit` defaults to
  the builder's `max_results` (default 5) and is clamped to the backend's max.
- `invoke`: call `backend.search(query, limit)`; a backend error ⇒ `Other`.
  Output: `{ "results": [ { "title", "url", "snippet", "content"? }, … ] }`.

### 7.1 Backends

Each backend builds its own `reqwest::Client` via `web::http::build_client()`.

- **`BraveBackend`** — `BraveBackend::new(api_key)` and
  `BraveBackend::from_env()` (reads `BRAVE_SEARCH_API_KEY`).
  `GET https://api.search.brave.com/res/v1/web/search?q=<query>&count=<limit>`
  with header `X-Subscription-Token: <key>`. Maps `web.results[]` →
  `{ title, url, description→snippet, content: None }`.
- **`TavilyBackend`** — `TavilyBackend::new(api_key)` and
  `TavilyBackend::from_env()` (reads `TAVILY_API_KEY`).
  `POST https://api.tavily.com/search` with JSON body
  `{ "api_key": <key>, "query": <query>, "max_results": <limit> }`. Maps
  `results[]` → `{ title, url, content→content, snippet = content truncated to
  ~200 chars }` (Tavily returns a `content` chunk per result, not a separate
  snippet field).

`from_env()` returns an error (surfaced by the caller) when the key var is
absent; the constructors do **not** read the network — they only build the
client and store the key.

## 8. Shared HTTP helper (`web/http.rs`, private)

- `build_client(user_agent: &str, timeout: Duration, redirect_policy) ->
  reqwest::Client` — single place configuring TLS (inherited from the workspace
  `reqwest` features), the user-agent, the timeout, and the redirect policy.
- `host_allowed(host, allow: &Option<Vec<String>>, deny: &[String]) -> bool` —
  the §6.1 matching logic, unit-tested directly.
- The `WebFetch` redirect policy is a `reqwest::redirect::Policy::custom`
  closure that re-applies `host_allowed` to each hop's target and aborts the
  redirect chain (yielding a request error) on a blocked host. Search backends
  use the default redirect policy (they only ever call their own fixed API
  host).

## 9. Dependencies & `deny.toml`

Add to root `[workspace.dependencies]`, reference via `dep.workspace = true`,
declare `optional = true` in the tools crate under the `web` feature:

- **`reqwest`** — reuse the existing workspace pin; in the tools crate request
  features `["json", "rustls", "stream"]` to **match
  `paigasus-helikon-providers-anthropic` exactly** so no second TLS stack enters
  the graph. `rustls` + `aws-lc-rs` + `rustls-platform-verifier` are already
  resolved in `Cargo.lock` and already pass cargo-deny. (The ticket's mention of
  `ring` is stale — the workspace resolves `aws-lc-rs`.)
- **`url`** — host extraction for the domain check.
- **`dom_smoothie`** — pure-Rust Readability main-content extraction. Verified
  on crates.io: latest `0.18` (**MIT**). Pin the current major; confirm MSRV
  (≤ 1.85) at implementation.
- **`htmd`** — pure-Rust HTML→Markdown. Verified on crates.io: latest `0.5`
  (**Apache-2.0**). Pin the current major; confirm MSRV at implementation.
  (Chosen over `html2md`, which is **GPL-3.0+** and would fail cargo-deny.)
- Reuse existing pins for `async-trait`, `serde`, `serde_json`, `schemars`,
  `anyhow`, `tokio`.

**`deny.toml` review (per the ticket):** the `reqwest`/`rustls`/`aws-lc-rs`
stack already passes (the provider crates shipped on it). Both new HTML crates
are MIT / Apache-2.0 — already on the `deny.toml` license allowlist — so **no
new license entry is expected**. The residual risk is whatever each pulls
transitively; run `cargo deny check` during implementation and add an allowlist
entry **only if it actually fails**. Commit the resulting `Cargo.lock` update.

## 10. Testing & the demo

Mirrors SMA-328's philosophy: deterministic CI with no network and no API keys;
the real-API path is a manual example.

- **`tests/web_fetch.rs`:**
  1. **Extraction is pure** — feed a fixture HTML string through the
     readability→markdown pipeline and assert the Markdown drops nav/script/style
     chrome and preserves the article body. No network.
  2. **Domain deny is pure** — a `WebFetchTool` with a deny-list returns
     `ToolError::Denied` *before* issuing any request, so this needs no server.
     Also assert a non-http(s) scheme ⇒ `Denied`. **(The AC.)**
  3. **Localhost round-trip** — a throwaway `tokio::net::TcpListener` serves a
     canned `200 text/html` response; assert the end-to-end fetch yields the
     expected Markdown, `status`, and final `url`. (Hand-rolled listener avoids a
     new dev-dep such as `wiremock`.)
- **`tests/web_search.rs`:**
  1. An in-crate **`ScriptedBackend` implementing `SearchBackend`** drives
     `WebSearchTool` and asserts the normalized `results` output — proving the
     tool wiring and runtime backend swappability with **no network**.
  2. **Parse-only unit tests** for `BraveBackend`/`TavilyBackend`: feed captured
     JSON response fixtures through each backend's wire→`SearchResult` mapping
     (the anthropic-SSE-fixture precedent) and assert the mapping, without
     hitting the live API. (The mapping is factored into a private free function
     per backend so it is testable without a live HTTP call.)
- **`examples/web_research.rs` (manual, not CI):**
  `OpenAiModel::chat("gpt-5-mini").build()?` equipped with `WebSearchTool` +
  `WebFetchTool`, behind `OPENAI_API_KEY` and `BRAVE_SEARCH_API_KEY` /
  `TAVILY_API_KEY`. It **installs a `PermissionPolicy`** (or `DenyRule`) over the
  web tools rather than running them wide open, doubling as the canonical "how to
  gate network tools" reference. `paigasus-helikon-providers-openai` stays a
  path-only dev-dependency (SMA-326 convention).
- **Fixture line endings:** the JSON/HTML fixtures are parsed by serde / the
  HTML parser (not byte-level literal-`\n` splits), so the `.gitattributes`
  `eol=lf` rule is **not** strictly required for them — but extend it for any
  fixture that test code splits byte-level (consistent with the existing
  anthropic-fixture convention).

## 11. Error model

Reuses the existing `ToolError` — **no core change**.

| Condition | `ToolError` variant |
|-----------|---------------------|
| Args fail schema / missing `url` or `query` | `InvalidArgs { schema_errors }` (recoverable) |
| Non-http(s) scheme; host blocked by allow/deny (incl. a redirect hop); non-text content-type | `Denied { reason }` |
| DNS/TLS/connect/timeout failure; search-backend API/transport error | `Other(anyhow::Error)` |
| non-2xx HTTP status; body truncation | **not errors** — reported in `ToolOutput` (`status`, `truncated`) |

`Denied` = a deliberate refusal (safety boundary or unsatisfiable
precondition). Operational/transport failures use `Other`. As established in
SMA-328 §7, the runner stringifies every `ToolError` uniformly into the tool
result today, so this taxonomy is about message clarity and future-proofing, not
current control flow.

## 12. Release mechanics

Plain additive `feat` — **no ascend ritual, no manual core/facade bump:**

- The tools crate is already `0.1.0` and published normally (no `publish =
  false`, no `release = false` block).
- No `paigasus-helikon-core` API is added (`ToolError::Denied` already exists).
- release-plz auto-bumps the tools crate on merge (additive ⇒ **patch** on a
  `0.x` crate: `0.1.0` → `0.1.1`) and, because release-plz itself performs the
  bump, its `dependencies_update` cascade updates the facade's dep pin and
  patch-bumps the facade automatically — the manual-bump drift caveat does
  **not** apply here.
- The only facade source change in the PR is the new `tools-web` feature line.
- `Cargo.lock` is committed (the workspace contains a binary); commit the lock
  update from the new deps.
- Branch: `feature/sma-412-paigasus-helikon-tools-webfetch-websearch-network-tools`.
  Design doc lands on the branch (not pre-merged to `main`).
- PR title (gated by `pr-title.yml`): full Conventional-Commits prefix +
  lowercase subject after the `SMA-###`, e.g.
  `feat(tools): SMA-412 add WebFetch + WebSearch network tools`.

## 13. Out of scope (YAGNI)

- Prompt-driven fetch summarization (needs a `Model`).
- SSRF / private-IP / loopback / link-local blocking (decision §2.3: pure
  allow/deny only; SSRF posture is the operator's via the runner).
- `robots.txt` honoring, rate-limiting, and response caching.
- Non-text extraction (PDF, images, etc.).
- A public shared `WebClient` primitive (Approach B — rejected for API
  minimalism).
- Splitting `web` into separate `web-fetch` / `web-search` features.
- Additional search backends beyond Brave + Tavily (the trait makes them a
  follow-up).

## 14. Acceptance criteria (restated against this design)

- An agent equipped with `WebFetchTool` can fetch an allowed URL (localhost
  round-trip test + the manual example) and is denied a blocked domain, surfaced
  as `ToolError::Denied { reason }` (`tests/web_fetch.rs`).
- `WebSearchTool` returns normalized results from at least one real backend
  (Brave **and** Tavily implemented; parse-tested against JSON fixtures; the
  manual example exercises a live backend), and the backend is swappable via the
  `SearchBackend` trait (`ScriptedBackend` test + `Arc<dyn SearchBackend>`).
- The web tools and `reqwest` are gated behind the off-by-default `web` feature
  (facade `tools-web`); a default `cargo build -p paigasus-helikon-tools` pulls
  no `reqwest`.
- All CI gates green (fmt, clippy `--all-features`, test matrix, docs,
  doc-coverage, commits, pr-title, audit, deny).

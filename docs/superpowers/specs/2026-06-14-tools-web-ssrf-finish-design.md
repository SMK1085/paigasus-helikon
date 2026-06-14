# SMA-417 — finish WebFetch SSRF hardening + WebSearch domain filter

**Status:** approved (ticket-prescribed; direct follow-up to SMA-412)
**Ticket:** [SMA-417](https://linear.app/smaschek/issue/SMA-417)
**Branch:** `feature/sma-417-paigasus-helikon-tools-finish-webfetch-ssrf-hardening`
**Date:** 2026-06-14
**Follows:** SMA-412 / PR #76 (shipped `-tools 0.1.1`). Target: `-tools 0.1.2`.

## 1. Summary

Close the four SMA-412 requirements that `0.1.1` did not ship (the SMA-412 ticket
was expanded after that work began). All four are prescribed by the updated
ticket; this doc records the implementation decisions. No `paigasus-helikon-core`
change. Additive `feat` → release-plz patch-bumps `-tools` `0.1.1 → 0.1.2` on
merge (no manual version edits).

## 2. Changes

### 2.1 Redirect cap ≤5
`MAX_REDIRECTS` in `web/fetch.rs`: `10 → 5`. The loop bound `0..=MAX_REDIRECTS`
already produces "too many redirects" past the cap. Add a test that a 6-hop
chain is denied.

### 2.2 Resolve-then-pin DNS (anti-rebinding)
A custom `reqwest::dns::Resolve` (`web::http::GuardedResolver`) installed on the
**WebFetch** client via `ClientBuilder::dns_resolver`. It resolves the host
(`tokio::net::lookup_host`), filters resolved IPs through the existing
`ip_blocked`, and returns only validated `SocketAddr`s — so reqwest connects to a
vetted IP (closes the TOCTOU between the pre-flight `ssrf_check` and connect). If
every resolved address is blocked (or none resolve) it errors, failing the
request.

- **`allow_private_ips` passthrough:** the resolver carries the flag; when set it
  returns all addresses unfiltered (so loopback/test fetches still work).
- **Literal-IP hosts:** reqwest does **not** invoke the resolver for numeric-IP
  hosts, so `http://169.254.169.254/` is still covered by the existing literal-IP
  branch in `ssrf_check` (unchanged). The pre-flight `ssrf_check` stays for the
  clean `Denied` error on the common case; the resolver is the connect-time pin.
- **Scope:** WebFetch only. Search backends keep the default resolver (they hit
  fixed public API hosts). `build_client` gains a `dns_guard: Option<bool>` param
  (`Some(allow_private)` ⇒ install `GuardedResolver`; `None` ⇒ default).

### 2.3 `max_uses` per-run fetch cap
`WebFetchToolBuilder::max_uses(usize)`; `WebFetchTool` stores `Option<usize>`
(default `None` = unlimited, backward-compatible). Enforced at the top of
`invoke` (before any network), run-scoped via `ToolContext::state()`:
read `uses` (u64) at key `paigasus_helikon_tools::web_fetch::uses`, deny with
`ToolError::Denied` if `>= max`, else `set(uses + 1)`. Run-scoped `SessionState`
resets per run; best-effort under concurrent sub-agents (a counter race can only
*under*-count by a hair — acceptable for an abuse cap).

### 2.4 WebSearch `allowed_domains` / `blocked_domains`
`WebSearchToolBuilder::{allowed_domains, blocked_domains}`; `WebSearchTool`
stores `Option<Vec<String>>` + `Vec<String>`. After the backend returns, filter
results by parsing each `result.url` and applying the existing
`web::http::host_allowed(host, allowed, blocked)` (same case-insensitive
suffix-match semantics as WebFetch). No filters configured ⇒ fast-path, no
filtering. A result whose URL is unparseable / host-less is dropped when any
filter is active.

## 3. Testing
- `web/http.rs`: `GuardedResolver` unit test — filters a blocked IP, passes a
  public IP, passthrough when `allow_private`. (Resolution itself is exercised
  via the integration tests.)
- `tests/web_fetch.rs`: 6-hop redirect chain (wiremock 302 loop) ⇒ `Denied`;
  `max_uses(1)` ⇒ 2nd invoke on a shared `ToolContext` ⇒ `Denied`. The existing
  literal-metadata-IP deny test stays green (literal path unchanged).
- `tests/web_search.rs`: `blocked_domains` drops a matching result;
  `allowed_domains` keeps only matching (driven by the `ScriptedBackend`).

## 4. Out of scope
The optional "only fetch URLs already in prior context" anti-exfiltration
guardrail (needs run-context plumbing) — separate follow-up.

## 5. Acceptance criteria
Per the ticket; verified by §3 tests + all CI gates green.

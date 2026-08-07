# AgentCore Image Size/Cold-Start Check Runbook

> **Scope:** operator procedure for `scripts/agentcore-image-check.sh` — building
> both `paigasus-helikon-runtime-agentcore` Docker images and validating the
> AgentCore size/cold-start acceptance criteria (SMA-332, spec §6.4) against them.
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling, matching
> `docs/runbooks/forkd-live-validation.md`'s precedent.
>
> **Validated 2026-07-06 on Docker Desktop 29.5.3, native arm64 macOS (no
> emulation).** Real numbers from that run:
>
> | Metric | Value | Gate |
> | --- | ---: | --- |
> | `helikon-agentcore-echo` image size (AC gate) | 1.31 MB | < 30 MB |
> | `helikon-agentcore-agent` image size (AC gate) | 3.27 MB | < 30 MB |
> | echo `exec`→`/ping`-200 (AC gate) | 11 ms | < 50 ms |
> | agent `exec`→`/ping`-200 (AC gate) | 9 ms | < 50 ms |
> | echo app-side log | `ready in 0ms` | — |
> | agent app-side log | `ready in 0ms` | — |
>
> Both gates passed with wide margin (agent image at ~11% of the size budget;
> cold start at ~20% of the latency budget).

## Prerequisites

- **Docker with BuildKit — now mandatory, not merely default.** The Dockerfile's
  builder stage uses `RUN --mount=type=cache` (SMA-457), which the legacy builder
  cannot parse. The script exports `DOCKER_BUILDKIT=1` itself, so a stale
  `DOCKER_BUILDKIT=0` in your environment cannot break it.
- **An arm64 host for a native build** (e.g. Apple Silicon macOS, or an arm64
  Linux box). The Dockerfile's builder stage is `rust:1.94-alpine`, published for
  both `amd64` and `arm64/v8` — on a non-arm64 host, add `docker buildx` with the
  `qemu` emulation binfmt handlers (`docker run --privileged --rm
  tonistiigi/binfmt --install arm64`) before running the script; the build will
  work but take noticeably longer under emulation. AgentCore's own runtime targets
  are `arm64` microVMs, hence `--platform linux/arm64` is hardcoded in both the
  Dockerfile's documented build commands and the check script — do not switch it
  to `amd64` for convenience.
- `curl` and `bash` ≥ 4 on the host running the script (macOS ships an old
  `/bin/bash` 3.2, but the script itself only needs POSIX-ish `bash` features
  present since 3.2; no `brew install bash` required, unlike
  `scripts/check-doc-coverage.sh`'s `mapfile` usage).
- Disk: the images themselves are a few MB each, but the BuildKit **cache mounts**
  (`$CARGO_HOME/registry` and the musl `target/` dir) persist between runs and are
  **never garbage-collected** — budget a few GB, and reclaim with
  `docker builder prune` when it grows. If a measurement ever looks wrong, re-run
  with `--no-cache`: cargo's freshness check is mtime-based against a tree
  supplied by `COPY . .`.
- GNU `date` on `PATH` (for `date +%s%N`). macOS's BSD `date` emits a literal `N`
  and the cold-start arithmetic breaks; `brew install coreutils` provides it.

## The one-liner

Run from the repository root:

```bash
bash scripts/agentcore-image-check.sh
```

This builds `helikon-agentcore-echo` (`EXAMPLE=echo_http`) and
`helikon-agentcore-agent` (`EXAMPLE=agent_http FEATURES=example-anthropic`) via
`crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile`, asserts both
images are under the 30 MB size gate, runs both containers and asserts each
one's `docker run`→first-`/ping`-200 latency is under 50 ms, and prints a
summary table. It exits non-zero (with the measured numbers on stderr) if any
of the four gates fails.

To build either image by hand — e.g. to `docker run` one interactively — see the
exact commands documented at the top of the Dockerfile itself.

### The cold-start gate is overridable; the size gate is not

`AGENTCORE_COLD_START_LIMIT_MS` raises the cold-start budget (default `50`).
Unlike image size, that number measures the *host* as much as the image —
container-start overhead is not comparable between a quiet developer machine and
a shared CI runner. Any override prints a loud
`NOTE: … this is NOT the AC value.` above the summary table, so a reader of the
output can never mistake the effective gate for the acceptance criterion.

There is deliberately **no** size override. The 30 MB gate carries the STOP RULE,
and an env knob on it would be exactly the quiet relaxation that rule exists to
prevent. (`AGENTCORE_SIZE_LIMIT_BYTES` is inert — setting it does nothing.)

CI runs this script from `.github/workflows/integration.yml`'s `agentcore-image`
job on `ubuntu-24.04-arm` with `AGENTCORE_COLD_START_LIMIT_MS=250`, as a
signal-only (non-required) check.

## Expected output

The script's final section looks like this (exact numbers vary run to run; the
table above captures one real validated run):

```text
== Summary ==
| Metric                           |          Value |       Gate |
| -------------------------------- | -------------- | ---------- |
| echo image size (AC gate)        |        1.31 MB |    < 30 MB |
| agent image size (AC gate)       |        3.27 MB |    < 30 MB |
| echo exec->200 (gate)            |           11 ms |    < 50 ms |
| agent exec->200 (gate)           |            9 ms |    < 50 ms |

echo image app-side log:  ...INFO paigasus_helikon_runtime_agentcore::server: ready in 0ms elapsed_ms=0
agent image app-side log: ...INFO paigasus_helikon_runtime_agentcore::server: ready in 0ms elapsed_ms=0

All gates passed.
```

## Acceptance-criteria interpretation note

Two different "cold start" numbers appear, and they measure different things:

- **App-side (`ready in {ms}ms`, logged by `AgentCoreServer::serve`/`serve_mcp`
  themselves)** — elapsed time from the server's `serve()`/`serve_mcp()` call to
  the TCP listener being bound. This is the number the AgentCore HTTP-protocol
  contract's cold-start guidance is actually about, and it is consistently ~0ms
  for both example binaries — process startup and listener bind are not where any
  real latency lives for a statically linked Rust binary.
- **External (`exec`→`/ping`-200, measured by this script)** — wall-clock time
  from container start (after `docker run -d` returns) to the first successful
  /ping response, as an *external* proxy/sanity-check for the same thing. This
  number is dominated by container runtime overhead (process creation, network
  namespace setup, Docker Desktop's host↔VM port forwarding on macOS) rather than
  anything the application controls, which is why the script measures it with a
  single retrying `curl` process rather than a shell polling loop — see the
  script's own comments for why a naive polling loop inflated this number by
  roughly 10x in early testing (measurement artifact, not real latency).
- **Neither number includes AWS's own microVM provisioning latency** (documented
  by AWS as roughly 2–5 seconds), which happens entirely on the platform side,
  before AgentCore ever execs the container's entrypoint. This script cannot
  measure that latency and does not claim to — it validates only the portion of
  cold start this crate's code can influence.

## A real bug this check caught: CA certificates in `scratch`

The first `agent_http` container run during this ticket's validation **panicked
on startup** with:

```text
thread 'main' (1) panicked at .../reqwest-0.13.4/src/async_impl/client.rs:2507:38:
Client::new(): reqwest::Error { kind: Builder, source: General("No CA certificates were loaded from the system") }
```

reqwest 0.13's `rustls` feature verifies TLS certificates via
`rustls-platform-verifier`, which on Linux loads the OS's native CA trust store —
and a `scratch` image has no filesystem beyond what's explicitly `COPY`'d in, so
there is no trust store to load. The Dockerfile fixes this by copying Alpine's
`ca-certificates` bundle into the final image and pointing
`rustls-native-certs` at it directly via `ENV SSL_CERT_FILE=...` (that variable
takes precedence over the crate's per-distro default-path guessing, so it is
correct regardless of which paths a bare `scratch` image happens to have
populated). See the Dockerfile's own comments for the full explanation. Verified
fixed: a real outbound HTTPS call to Anthropic's API (with a deliberately invalid
key) now returns a proper `401`-shaped API error instead of crashing the process.

If you are adapting this Dockerfile for a different provider/example that makes
outbound HTTPS calls, keep this `COPY`+`ENV` pair; if you are certain an example
never makes an outbound TLS connection (like `echo_http`), it is harmless but
unnecessary weight (~220 KB, well within the size budget either way).

## ECR push + CDK

Publishing the image to Amazon ECR and wiring it into an
`aws-cdk-lib/aws-bedrockagentcore` `Runtime` construct (including the MCP-mode
`protocolConfiguration: ProtocolType.MCP` variant) is documented in the crate's
own README (`crates/paigasus-helikon-runtime-agentcore/README.md`) rather than
duplicated here — that README is this crate's crates.io/docs.rs landing page and
is the canonical place for downstream-consumer-facing deployment instructions.
(Per the project's SDD plan, populating that README is a separate, later ticket;
this runbook exists so the size/cold-start evidence has a home in the meantime.)

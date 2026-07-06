# AWS Bedrock AgentCore container image for a `paigasus-helikon-runtime-agentcore`
# example binary. Multi-stage: a musl builder produces a statically linked binary,
# which then ships alone in a `scratch` image (no libc, no shell, nothing else).
#
# Build context MUST be the workspace root (this is a Cargo workspace — the builder
# stage needs every member crate's Cargo.toml + src/, not just this one crate's
# directory). Run from the repository root:
#
#   # Minimal-overhead image (no model provider, no TLS stack):
#   docker build --platform linux/arm64 \
#     -f crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile \
#     --build-arg EXAMPLE=echo_http \
#     -t helikon-agentcore-echo .
#
#   # Model-backed image (the size/cold-start acceptance-criteria image):
#   docker build --platform linux/arm64 \
#     -f crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile \
#     --build-arg EXAMPLE=agent_http --build-arg FEATURES=example-anthropic \
#     -t helikon-agentcore-agent .
#
# See scripts/agentcore-image-check.sh for both builds plus the size/cold-start
# acceptance checks, and docs/runbooks/agentcore-image-check.md for the full
# runbook.

# rust:1.94-alpine is the base image tag closest to (and satisfying) this
# workspace's MSRV (1.94, see root CLAUDE.md); confirmed to exist for both amd64
# and arm64/v8 via `docker manifest inspect rust:1.94-alpine` at implementation
# time. Not pinned to a base-image digest: the tag already carries the exact Rust
# version we need, and `--locked` below still pins every crate dependency via
# Cargo.lock regardless of which Alpine point release backs the tag.
FROM rust:1.94-alpine AS builder

# aws-lc-rs (pulled in by the Anthropic provider's `reqwest`/rustls stack, and
# transitively by rmcp's reqwest transport) builds C/assembly code at compile time
# under musl and needs cmake + a C toolchain + perl (for its bundled BoringSSL-style
# build scripts) + go (for some of its codegen); musl-dev/build-base give the base C
# toolchain musl needs beyond what the image ships by default. ca-certificates
# supplies the CA bundle copied into the final `scratch` image below — see that
# COPY's comment for why a `scratch` image needs one at all.
RUN apk add --no-cache musl-dev build-base cmake perl go ca-certificates

# No-op on this (already-musl, already-native-arch) base image — the host target
# already satisfies aarch64-unknown-linux-musl — but kept explicit so this
# Dockerfile documents the actual target being produced and stays correct if ever
# adapted to cross-compile from a non-musl builder.
RUN rustup target add aarch64-unknown-linux-musl

WORKDIR /workspace
COPY . .

ARG EXAMPLE=echo_http
ARG FEATURES=

# `${FEATURES:+--features "${FEATURES}"}` is POSIX shell parameter expansion
# (supported by alpine's /bin/sh): expands to nothing when FEATURES is unset or
# empty (the echo_http build), and to `--features example-anthropic` when set (the
# agent_http build). Docker exposes declared ARGs as this RUN step's shell
# environment, so the expansion sees the --build-arg value directly.
RUN cargo build --release --locked -p paigasus-helikon-runtime-agentcore \
    --example "${EXAMPLE}" ${FEATURES:+--features "${FEATURES}"} \
    && strip "target/release/examples/${EXAMPLE}" \
    && cp "target/release/examples/${EXAMPLE}" /agentcore-example

# No libc, no shell, no package manager — just the statically linked musl binary
# (+ a CA bundle; see below).
FROM scratch
COPY --from=builder /agentcore-example /agentcore-example

# `agent_http` makes outbound HTTPS calls to Anthropic's API; reqwest 0.13's
# `rustls` feature verifies server certificates via `rustls-platform-verifier`,
# which on Linux loads the OS's native CA trust store — something a `scratch`
# image has none of. Without this, `AnthropicModel::messages(...).build()`
# panics at startup with "No CA certificates were loaded from the system" (verified
# empirically: this crashed the very first `agent_http` container run during this
# ticket's image-size validation). Fix: copy Alpine's `ca-certificates` bundle in
# and point `rustls-native-certs` at it directly via `SSL_CERT_FILE` — that env var
# takes precedence over `rustls-native-certs`'s own per-distro default-path
# guessing, so it works regardless of which paths a `scratch` image happens to
# have populated. `echo_http` makes no outbound calls and never touches this, but
# the ~220 KB bundle costs nothing relative to the 30 MB size gate either way.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt

# 8080: the AgentCore HTTP-protocol contract's fixed port (AgentCoreServer::serve).
# 8000: MCP-protocol mode's fixed port (AgentCoreServer::serve_mcp, feature `mcp`).
# A given example binary only ever listens on one of the two at a time; both are
# declared so the same Dockerfile documents either mode's contract.
EXPOSE 8080 8000

ENTRYPOINT ["/agentcore-example"]

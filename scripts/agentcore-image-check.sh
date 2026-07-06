#!/usr/bin/env bash
# SMA-332: build both paigasus-helikon-runtime-agentcore Docker images and check
# the AgentCore size/cold-start acceptance criteria against them.
#
#   bash scripts/agentcore-image-check.sh
#
# See docs/runbooks/agentcore-image-check.md for prerequisites, expected output,
# and the acceptance-criteria interpretation notes this script's assertions rely on.
#
# Builds two images from crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile
# (build context = workspace root):
#   - helikon-agentcore-echo:  EXAMPLE=echo_http           — no model provider, no TLS.
#   - helikon-agentcore-agent: EXAMPLE=agent_http FEATURES=example-anthropic
#                              — the size/cold-start acceptance-criteria image.
#
# Gates (STOP RULE, spec §6.4): the agent image must be < 30 MB. If it is not —
# or if the build itself fails (e.g. aws-lc-rs won't build under musl) — this
# script exits non-zero with the measured numbers/build errors so a human can
# decide the recorded fallback; it does not silently substitute the echo image's
# size for the agent image's.
#
# All four measured metrics are AC-gated (see the SIZE_LIMIT_BYTES/
# COLD_START_LIMIT_MS checks below): both images' size and both images'
# exec->/ping cold start. The echo image also demonstrates the framework's own
# minimal-overhead footprint, but that is in addition to — not instead of —
# being checked against the same gates as the agent image.
#
# Cold start: runs the container and measures wall-clock time from container
# start (after `docker run -d` returns) to the first successful `GET /ping` 200,
# using a single `curl` process with its own zero-delay retry loop (`--retry
# --retry-delay 0 --retry-connrefused`) rather than a shell polling loop — a bash
# loop that spawns a fresh `curl` process per attempt adds tens of milliseconds of
# process-spawn overhead per iteration on macOS, which would swamp the sub-50ms
# budget with measurement noise rather than real latency (empirically: ~100ms via a
# 5ms-interval spawn-per-attempt loop vs. ~10ms via this single-process retry
# loop, for the exact same container). This is still an external, container-start-
# to-first-response measurement — it is not the same as the binary's own internal
# "ready in {ms}ms" log (see the runbook's AC-interpretation note for why both
# numbers are reported and what each one means).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DOCKERFILE="crates/paigasus-helikon-runtime-agentcore/docker/Dockerfile"

ECHO_IMAGE="helikon-agentcore-echo"
AGENT_IMAGE="helikon-agentcore-agent"
ECHO_CONTAINER="agentcore-image-check-echo"
AGENT_CONTAINER="agentcore-image-check-agent"
HOST_PORT_ECHO="18080"
HOST_PORT_AGENT="18081"

# The AC gate (spec §6.4 / task brief): both the model-backed image and the
# echo image must stay under this many bytes — see the module doc comment above.
SIZE_LIMIT_BYTES=$((30 * 1024 * 1024))
# Cold-start gate: exec (docker run) → first `/ping` 200, in milliseconds.
COLD_START_LIMIT_MS=50

cd "${REPO_ROOT}"

cleanup() {
  docker rm -f "${ECHO_CONTAINER}" "${AGENT_CONTAINER}" > /dev/null 2>&1 || true
}
trap cleanup EXIT

echo "== Building ${ECHO_IMAGE} (EXAMPLE=echo_http) =="
docker build --platform linux/arm64 \
  -f "${DOCKERFILE}" \
  --build-arg EXAMPLE=echo_http \
  -t "${ECHO_IMAGE}" \
  .

echo
echo "== Building ${AGENT_IMAGE} (EXAMPLE=agent_http FEATURES=example-anthropic) =="
docker build --platform linux/arm64 \
  -f "${DOCKERFILE}" \
  --build-arg EXAMPLE=agent_http \
  --build-arg FEATURES=example-anthropic \
  -t "${AGENT_IMAGE}" \
  .

echo_size=$(docker image inspect "${ECHO_IMAGE}" --format '{{.Size}}')
agent_size=$(docker image inspect "${AGENT_IMAGE}" --format '{{.Size}}')

# Wall-clock, docker-run-to-first-200 latency via a container's exposed host port
# and a single retrying `curl` process (see the module doc comment for why).
# Prints the measured milliseconds on stdout; the container is left running for
# the caller to inspect/remove.
measure_cold_start_ms() {
  local image="$1" container="$2" host_port="$3"
  shift 3

  docker rm -f "${container}" > /dev/null 2>&1 || true
  docker run -d --name "${container}" -p "${host_port}:8080" "$@" "${image}" > /dev/null

  local start_ns end_ns
  start_ns=$(date +%s%N)
  if ! curl -s -o /dev/null --fail \
      --retry 400 --retry-delay 0 --retry-all-errors --retry-connrefused \
      --max-time 10 \
      "http://localhost:${host_port}/ping"; then
    echo "FAILED: ${container} never answered /ping with 2xx within the retry budget" >&2
    docker logs "${container}" >&2 || true
    exit 1
  fi
  end_ns=$(date +%s%N)

  echo $(( (end_ns - start_ns) / 1000000 ))
}

echo
echo "== Measuring cold start (echo image) =="
echo_cold_start_ms=$(measure_cold_start_ms "${ECHO_IMAGE}" "${ECHO_CONTAINER}" "${HOST_PORT_ECHO}")
echo_ready_log=$(docker logs "${ECHO_CONTAINER}" 2>&1 | grep -m1 "ready in" || echo "(not found in container logs)")

echo
echo "== Measuring cold start (agent image, AC gate) =="
agent_cold_start_ms=$(measure_cold_start_ms "${AGENT_IMAGE}" "${AGENT_CONTAINER}" "${HOST_PORT_AGENT}" \
  -e ANTHROPIC_API_KEY=sk-agentcore-image-check-placeholder)
agent_ready_log=$(docker logs "${AGENT_CONTAINER}" 2>&1 | grep -m1 "ready in" || echo "(not found in container logs)")

to_mb() {
  awk -v b="$1" 'BEGIN { printf "%.2f", b / (1024*1024) }'
}

echo
echo "== Summary =="
printf '| %-32s | %14s | %10s |\n' "Metric" "Value" "Gate"
printf '| %-32s | %14s | %10s |\n' "--------------------------------" "--------------" "----------"
printf '| %-32s | %11s MB | %10s |\n' "echo image size (AC gate)" "$(to_mb "${echo_size}")" "< 30 MB"
printf '| %-32s | %11s MB | %10s |\n' "agent image size (AC gate)" "$(to_mb "${agent_size}")" "< 30 MB"
printf '| %-32s | %12s ms | %10s |\n' "echo exec->200 (AC gate)" "${echo_cold_start_ms}" "< 50 ms"
printf '| %-32s | %12s ms | %10s |\n' "agent exec->200 (AC gate)" "${agent_cold_start_ms}" "< 50 ms"
echo
echo "echo image app-side log:  ${echo_ready_log}"
echo "agent image app-side log: ${agent_ready_log}"
echo

failed=0

if [[ "${echo_size}" -ge "${SIZE_LIMIT_BYTES}" ]]; then
  echo "FAILED: echo image is $(to_mb "${echo_size}") MB, >= the 30 MB AC gate." >&2
  failed=1
fi

if [[ "${agent_size}" -ge "${SIZE_LIMIT_BYTES}" ]]; then
  echo "FAILED: agent image is $(to_mb "${agent_size}") MB, >= the 30 MB AC gate." >&2
  echo "        STOP RULE: do not silently fall back to gating on the echo image." >&2
  echo "        Report BLOCKED with this measured size; the fallback (spec §6.4) needs sign-off." >&2
  failed=1
fi

if [[ "${echo_cold_start_ms}" -ge "${COLD_START_LIMIT_MS}" ]]; then
  echo "FAILED: echo image exec->200 cold start is ${echo_cold_start_ms}ms, >= the ${COLD_START_LIMIT_MS}ms gate." >&2
  failed=1
fi

if [[ "${agent_cold_start_ms}" -ge "${COLD_START_LIMIT_MS}" ]]; then
  echo "FAILED: agent image exec->200 cold start is ${agent_cold_start_ms}ms, >= the ${COLD_START_LIMIT_MS}ms gate." >&2
  failed=1
fi

if [[ "${failed}" -ne 0 ]]; then
  exit 1
fi

echo "All gates passed."

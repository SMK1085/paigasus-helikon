#!/usr/bin/env bash
# Assert that the protoc later steps will actually use is the pinned one.
#
# This MUST run as its own composite step. GITHUB_PATH and GITHUB_ENV writes do
# not affect the step that makes them, only later steps — so an assertion living
# inside install.sh would validate a local `export PATH=`, not the mechanism
# cargo sees, and would be structurally blind to the propagation failure it
# exists to catch.
set -euo pipefail

EXPECTED_VERSION="libprotoc 35.1"

if [ -z "${PROTOC:-}" ]; then
  echo "::error::PROTOC is unset — install.sh did not export it, or GITHUB_ENV did not propagate."
  exit 1
fi

# PROTOC is a Win32 path on Windows; bash needs the POSIX form to exec it.
if [ "${RUNNER_OS:-}" = "Windows" ]; then
  protoc_bin="$(cygpath -u "${PROTOC}")"
  include_dir="$(cygpath -u "${PROTOC_INCLUDE}")"
else
  protoc_bin="${PROTOC}"
  include_dir="${PROTOC_INCLUDE}"
fi

# Windows protoc emits a trailing CR; a bare compare fails on a CORRECT install.
exported_version="$("${protoc_bin}" --version | tr -d '\r')"
resolved="$(command -v protoc || true)"

echo "setup-protoc: PROTOC   ${PROTOC}"
echo "setup-protoc: resolved ${resolved}"
echo "setup-protoc: version  ${exported_version}"

# 1. The compiler prost-build will actually use is the pinned one.
if [ "${exported_version}" != "${EXPECTED_VERSION}" ]; then
  echo "::error::PROTOC reports '${exported_version}', expected '${EXPECTED_VERSION}'."
  exit 1
fi

# 2. The PATH fallback resolves, and resolves to the same version. A version
#    check alone would pass if some other 35.1 protoc were first on PATH, so
#    the resolved location is logged for exactly that case.
if [ -z "${resolved}" ]; then
  echo "::error::protoc is not on PATH — GITHUB_PATH did not propagate."
  exit 1
fi

path_version="$(protoc --version | tr -d '\r')"
if [ "${path_version}" != "${EXPECTED_VERSION}" ]; then
  echo "::error::PATH resolves protoc to '${path_version}' at ${resolved}, expected '${EXPECTED_VERSION}'."
  exit 1
fi

# 3. The well-known types survived extraction. Without this sibling tree every
#    google/protobuf/*.proto import fails — which is every temporal proto.
if [ ! -f "${include_dir}/google/protobuf/timestamp.proto" ]; then
  echo "::error::well-known types missing at ${include_dir}/google/protobuf/ — extraction dropped the include tree."
  exit 1
fi

echo "setup-protoc: ok"

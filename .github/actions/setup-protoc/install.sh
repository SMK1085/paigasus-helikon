#!/usr/bin/env bash
# Install a pinned, checksum-verified protoc and expose it to later steps.
#
# Runner contract — all read from the environment, all fakeable locally, which
# is how selftest.sh exercises this file:
#   RUNNER_OS, RUNNER_ARCH  select the release asset
#   RUNNER_TEMP             scratch space for the download
#   GITHUB_PATH             file; each line appended is prepended to PATH
#   GITHUB_ENV              file; each KEY=VALUE appended is exported
#
# Order is load-bearing: download -> verify -> extract -> export. An unverified
# archive must never reach an executable location.
set -euo pipefail

PROTOC_VERSION="35.1"

# Hand-bumped; nothing tracks these automatically. Dependabot follows action
# SHAs, and after SMA-458 there is no third-party action here for it to follow.
# Derived 2026-08-13 with `shasum -a 256` from:
#   https://github.com/protocolbuffers/protobuf/releases/tag/v35.1
# Bump runbook: CLAUDE.md, "protoc pin". Re-verify with selftest.sh.
SHA256_LINUX_X64="6930ebf62bd4ea607b98fff052596c6ee564b9835b4ce172c75a3f53ae9d91b7"
SHA256_MACOS_ARM64="193289af0470c6a1aada357d4fba0bbf8d78bfaac8b5e42ca30af2ef75583de2"
SHA256_WINDOWS_X64="5d3ff218d7d91eea95f7569bcb5a98f3030f8996d44151279d9772edcff76082"

case "${RUNNER_OS}-${RUNNER_ARCH}" in
  Linux-X64)   asset="linux-x86_64"; expected="${SHA256_LINUX_X64}";   exe="protoc"     ;;
  macOS-ARM64) asset="osx-aarch_64"; expected="${SHA256_MACOS_ARM64}"; exe="protoc"     ;;
  Windows-X64) asset="win64";        expected="${SHA256_WINDOWS_X64}"; exe="protoc.exe" ;;
  *)
    echo "::error::setup-protoc has no asset for ${RUNNER_OS}-${RUNNER_ARCH}. Add the asset name and its SHA-256 to .github/actions/setup-protoc/install.sh."
    exit 1
    ;;
esac

# RUNNER_TEMP is a Win32 path on Windows runners; bash needs the POSIX form.
if [ "${RUNNER_OS}" = "Windows" ]; then
  temp="$(cygpath -u "${RUNNER_TEMP}")"
else
  temp="${RUNNER_TEMP}"
fi

root="${temp}/protoc-${PROTOC_VERSION}"
archive="${temp}/protoc-${PROTOC_VERSION}-${asset}.zip"
rm -rf "${root}"
mkdir -p "${root}"

# The tag carries a leading v (v35.1); the filename does not (protoc-35.1-...).
# Mixing them 404s.
url="https://github.com/protocolbuffers/protobuf/releases/download/v${PROTOC_VERSION}/protoc-${PROTOC_VERSION}-${asset}.zip"

echo "setup-protoc: asset    ${asset}"
echo "setup-protoc: url      ${url}"

# Retries are not decoration: this runs in 11 job executions per PR, nine of
# them behind required contexts, plus the crates.io publish path.
curl -sSfL --retry 3 --retry-all-errors --retry-delay 2 \
     --connect-timeout 15 --max-time 300 \
     -o "${archive}" "${url}"

# macOS has no sha256sum; Linux runners and Git Bash do. Both accept the same
# "DIGEST  PATH" output format.
if command -v sha256sum > /dev/null 2>&1; then
  actual="$(sha256sum "${archive}" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "${archive}" | awk '{print $1}')"
fi

echo "setup-protoc: expected ${expected}"
echo "setup-protoc: actual   ${actual}"

if [ "${actual}" != "${expected}" ]; then
  echo "::error::protoc checksum mismatch for ${asset}. Likely causes, in order: (1) truncated or corrupted download; (2) the upstream release was re-tagged or its asset replaced; (3) tampering. DO NOT update the digest in install.sh without independently verifying the upstream release."
  exit 1
fi

# unzip preserves the archive's Unix mode. Expand-Archive and python zipfile do
# not, and the resulting non-executable protoc fails with "Permission denied"
# deep inside a cargo build script, far from here. 7z ships on the Windows
# runner image; unzip is not guaranteed in Git Bash.
if [ "${RUNNER_OS}" = "Windows" ]; then
  7z x -y -o"${root}" "${archive}" > /dev/null
else
  unzip -q "${archive}" -d "${root}"
  chmod +x "${root}/bin/${exe}"
fi

bin_dir="${root}/bin"
protoc_bin="${bin_dir}/${exe}"
include_dir="${root}/include"

# GITHUB_PATH and GITHUB_ENV are consumed by the runner, not by bash. On Windows
# a POSIX /d/a/... entry is a dead PATH entry to CreateProcess when cargo spawns
# protoc: it looks correct from inside bash and fails everywhere else.
if [ "${RUNNER_OS}" = "Windows" ]; then
  bin_dir="$(cygpath -w "${bin_dir}")"
  protoc_bin="$(cygpath -w "${protoc_bin}")"
  include_dir="$(cygpath -w "${include_dir}")"
fi

echo "${bin_dir}" >> "${GITHUB_PATH}"

# prost-build resolves PROTOC from the environment before falling back to a PATH
# lookup, so exporting it makes this install authoritative regardless of PATH
# ordering. PROTOC_INCLUDE carries the well-known types (google/protobuf/*.proto)
# that protoc would otherwise resolve relative to its own binary.
{
  echo "PROTOC=${protoc_bin}"
  echo "PROTOC_INCLUDE=${include_dir}"
} >> "${GITHUB_ENV}"

echo "setup-protoc: installed ${protoc_bin}"

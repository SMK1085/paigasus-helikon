#!/usr/bin/env bash
# Local self-test for the setup-protoc composite action.
#
# NOT run by CI — in CI the action itself is the test. This exists so the install
# logic can be exercised, and a version bump re-verified, without pushing a
# commit. Run it after changing PROTOC_VERSION or any digest.
#
#   bash .github/actions/setup-protoc/selftest.sh
#
# Requires network access. The Windows branch of install.sh cannot be exercised
# here (it needs cygpath and 7z) — CI's `test (windows-latest, stable)` is the
# only thing covering it, and that job is NOT a required context, so confirm it
# by hand before merging.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }

# assert <description> <command...> — describe the property, not the outcome, so
# the same string reads correctly under both ok and FAIL. Deliberately not the
# `cond && pass || fail` idiom, which reports a false FAIL if pass itself fails.
assert() {
  local desc="$1"; shift
  if "$@"; then pass "${desc}"; else fail "${desc}"; fi
}

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) native_os=macOS; native_arch=ARM64 ;;
  Linux-x86_64) native_os=Linux; native_arch=X64   ;;
  *) echo "selftest: no native mapping for $(uname -s)-$(uname -m)"; exit 1 ;;
esac

version="$(awk -F'"' '/^PROTOC_VERSION=/ {print $2}' "${here}/install.sh")"
echo "selftest: install.sh pins protoc ${version}"

# --- 1. every pinned digest still matches the published asset ----------------
echo "1. pinned digests match published assets"
while read -r var asset; do
  expected="$(awk -F'"' -v pat="^${var}=" '$0 ~ pat {print $2}' "${here}/install.sh")"
  tmp="$(mktemp -d)"
  url="https://github.com/protocolbuffers/protobuf/releases/download/v${version}/protoc-${version}-${asset}.zip"
  if curl -sSfL --retry 3 -o "${tmp}/a.zip" "${url}"; then
    actual="$(sha256_of "${tmp}/a.zip")"
    if [ "${actual}" = "${expected}" ]; then
      pass "${asset}"
    else
      fail "${asset}: pinned ${expected}, published ${actual}"
    fi
  else
    fail "${asset}: download failed (${url})"
  fi
  rm -rf "${tmp}"
done <<'ASSETS'
SHA256_LINUX_X64 linux-x86_64
SHA256_MACOS_ARM64 osx-aarch_64
SHA256_WINDOWS_X64 win64
ASSETS

# --- 2. native install, then the real verify.sh must pass --------------------
echo "2. native install + verify (${native_os}-${native_arch})"
t2="$(mktemp -d)"
if env RUNNER_OS="${native_os}" RUNNER_ARCH="${native_arch}" RUNNER_TEMP="${t2}" \
       GITHUB_PATH="${t2}/gh_path" GITHUB_ENV="${t2}/gh_env" \
       bash "${here}/install.sh" > "${t2}/install.log" 2>&1; then
  pass "install.sh exited 0"
else
  fail "install.sh failed"; sed 's/^/      /' "${t2}/install.log"
fi

# Reproduce what the runner does between steps: GITHUB_ENV lines become the next
# step's environment, GITHUB_PATH lines are prepended to PATH.
if [ -s "${t2}/gh_env" ] && [ -s "${t2}/gh_path" ]; then
  set -a
  # shellcheck source=/dev/null
  . "${t2}/gh_env"
  set +a
  PATH="$(head -n 1 "${t2}/gh_path"):${PATH}"; export PATH
  if env RUNNER_OS="${native_os}" bash "${here}/verify.sh" > "${t2}/verify.log" 2>&1; then
    pass "verify.sh exited 0"
  else
    fail "verify.sh failed"; sed 's/^/      /' "${t2}/verify.log"
  fi
else
  fail "install.sh exported nothing to GITHUB_ENV/GITHUB_PATH"
fi

# --- 3. cross-platform install lays down bin/ + include/ ---------------------
# Exercises a non-native digest and extraction path. The binary is not executed.
if [ "${native_os}" != "Linux" ]; then
  echo "3. cross-platform install (Linux-X64)"
  t3="$(mktemp -d)"
  if env RUNNER_OS=Linux RUNNER_ARCH=X64 RUNNER_TEMP="${t3}" \
         GITHUB_PATH="${t3}/gh_path" GITHUB_ENV="${t3}/gh_env" \
         bash "${here}/install.sh" > "${t3}/install.log" 2>&1; then
    pass "install.sh exited 0"
  else
    fail "install.sh failed"; sed 's/^/      /' "${t3}/install.log"
  fi
  assert "bin/protoc present" \
    test -f "${t3}/protoc-${version}/bin/protoc"
  assert "include tree present" \
    test -f "${t3}/protoc-${version}/include/google/protobuf/timestamp.proto"
  assert "executable bit set" \
    test -x "${t3}/protoc-${version}/bin/protoc"
else
  echo "3. cross-platform install — skipped (native host is already Linux-X64)"
fi

# --- 4. a bad digest must fail closed ---------------------------------------
# Asserts the security property, not just the exit code: on a mismatch nothing
# is exported AND nothing is extracted. That is what proves the
# verify-before-extract ordering actually holds.
echo "4. tampered digest fails closed"
t4="$(mktemp -d)"
zeros="$(printf '%064d' 0)"
sed -E "s/\"[0-9a-f]{64}\"/\"${zeros}\"/" "${here}/install.sh" > "${t4}/tampered.sh"
env RUNNER_OS="${native_os}" RUNNER_ARCH="${native_arch}" RUNNER_TEMP="${t4}" \
    GITHUB_PATH="${t4}/gh_path" GITHUB_ENV="${t4}/gh_env" \
    bash "${t4}/tampered.sh" > "${t4}/out.log" 2>&1
rc=$?
assert "exits non-zero on a bad digest" test "${rc}" -ne 0
assert "reports a checksum mismatch"    grep -q "checksum mismatch" "${t4}/out.log"
assert "exports nothing to GITHUB_ENV"  test ! -s "${t4}/gh_env"
assert "exports nothing to GITHUB_PATH" test ! -s "${t4}/gh_path"
assert "extracts nothing"               test ! -e "${t4}/protoc-${version}/bin"

# --- 5. a decoy protoc earlier on PATH must be rejected ---------------------
# Guards identity, not just version: the decoy reports the pinned version
# string, so a version-only check would pass here. This case fails unless
# verify.sh compares the RESOLVED path against the install. Depends on case 2
# having exported PROTOC/PROTOC_INCLUDE into this shell.
echo "5. decoy protoc on PATH is rejected"
if [ -n "${PROTOC:-}" ]; then
  t5="$(mktemp -d)"
  mkdir -p "${t5}/decoy"
  printf '#!/usr/bin/env bash\necho "libprotoc 35.1"\n' > "${t5}/decoy/protoc"
  chmod +x "${t5}/decoy/protoc"
  env RUNNER_OS="${native_os}" PATH="${t5}/decoy:${PATH}" \
      bash "${here}/verify.sh" > "${t5}/out.log" 2>&1
  rc=$?
  assert "exits non-zero when PATH resolves elsewhere" test "${rc}" -ne 0
  assert "error names the PATH export" grep -q "GITHUB_PATH export did not take effect" "${t5}/out.log"
else
  fail "case 2 did not export PROTOC; cannot run the decoy check"
fi

# --- 6. an unsupported platform fails loudly --------------------------------
echo "6. unsupported platform fails loudly"
t6="$(mktemp -d)"
env RUNNER_OS=Plan9 RUNNER_ARCH=X64 RUNNER_TEMP="${t6}" \
    GITHUB_PATH="${t6}/gh_path" GITHUB_ENV="${t6}/gh_env" \
    bash "${here}/install.sh" > "${t6}/out.log" 2>&1
rc=$?
assert "exits non-zero on an unknown platform" test "${rc}" -ne 0
assert "error names the file to edit"          grep -q "install.sh" "${t6}/out.log"

echo
if [ "${failures}" -eq 0 ]; then
  echo "selftest: PASS"
else
  echo "selftest: FAIL (${failures})"
  exit 1
fi

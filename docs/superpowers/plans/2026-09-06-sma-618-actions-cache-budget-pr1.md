# SMA-618 Actions Cache Budget — PR 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop PR jobs from writing GitHub Actions caches, remove the wasted target caches on `audit`/`deny`, and add the two guards (env-drift script, daily budget monitor) that keep the fix from silently regressing.

**Architecture:** Purely CI configuration. Twelve `Swatinem/rust-cache` sites across seven workflows gain `save-if: ${{ github.ref == 'refs/heads/main' }}` so PR runs restore but never save. `audit`/`deny` additionally get `cache-targets: false` because neither compiles the workspace. A new bash guard script asserts the cargo-visible environment stays uniform across cache-bearing workflows (those variables are inside the cache key), and a new schedule-only workflow reports cache usage daily so the budget is enforceable rather than aspirational.

**Tech Stack:** GitHub Actions YAML, bash (repo idiom: `set -euo pipefail`, header comment explaining *why* the script exists), `gh` CLI.

**Spec:** `docs/superpowers/specs/2026-09-06-sma-618-actions-cache-budget-design.md`

## Global Constraints

- Commit prefix: `<type>(<scope>): SMA-618 <message>`. Subject starts lowercase. `convco` runs in the `commit-msg` hook.
- **No job may be renamed.** Required contexts in `.github/rulesets/main-protection-checks.json` are bare job names.
- Actions stay SHA-pinned with `# <owner>/<action> vX.Y.Z` on the line above. `Swatinem/rust-cache` stays at `6323deb102c322ba6fcbdcafc7e3dddab59af2b6` (v2.9.2) — **only its inputs change.**
- `save-if` expression is the literal `${{ github.ref == 'refs/heads/main' }}` at every site. Do not substitute `github.event.repository.default_branch`.
- Workflow-level `env:` blocks in cache-bearing workflows must stay byte-identical — that is what Task 1's guard enforces.
- **Out of scope for PR 1:** `CARGO_PROFILE_DEV_DEBUG`, `shared-key`, `cache-targets: false` anywhere except `audit`/`deny`, `prefix-key: v1`, the Windows `trybuild_ui` skip. All are PR 2.
- markdownlint runs on `**/*.md` except `docs/superpowers/**`. Docs edits in Task 5 touch `CLAUDE.md`, `CONTRIBUTING.md` and `docs/runbooks/ci-architecture.md`, which **are** linted — run `npx markdownlint-cli2` before committing Task 5. Node ≥ 22 required (`~/.nvm/versions/node/v24.20.0/bin` on this host; the default `node` is v18 and crashes markdownlint).

## File Structure

| File | Responsibility |
| -- | -- |
| `scripts/check-cargo-profile-env-sync.sh` | **New.** Asserts cargo-visible env uniformity across cache-bearing workflows (assertions 1–2). |
| `scripts/check-cargo-profile-env-sync-selftest.sh` | **New.** Pins the parser's contract against synthetic fixtures. Naming follows `.github/actions/setup-protoc/selftest.sh`. |
| `.github/workflows/cache-budget.yml` | **New.** Schedule-only cache usage report. Separate workflow, not a job in `audit.yml` — see Task 4. |
| `.github/workflows/ci.yml` | 6 cache sites get `save-if`; `fmt` gains the guard step. |
| `.github/workflows/msrv.yml`, `bench.yml` | Gain a workflow-level `env:` block; cache site gets `save-if`. |
| `.github/workflows/audit.yml`, `deny.yml` | Cache site gets `save-if` + `cache-targets: false`. |
| `.github/workflows/sbom.yml`, `integration.yml` | Cache site gets `save-if`. |
| `CLAUDE.md`, `CONTRIBUTING.md`, `docs/runbooks/ci-architecture.md` | Record the budget, the rules, the new script, and fix a false claim. |

---

### Task 1: The env-drift guard script

The guard is written first, and self-tested, because Task 2 wires it into a
required job — a broken parser there blocks every PR.

**Files:**
- Create: `scripts/check-cargo-profile-env-sync.sh`
- Create: `scripts/check-cargo-profile-env-sync-selftest.sh`

**Interfaces:**
- Consumes: nothing.
- Produces: `bash scripts/check-cargo-profile-env-sync.sh [WORKFLOW_DIR]` — exits 0 on agreement, 1 on drift, 2 on a usage/environment error. `WORKFLOW_DIR` defaults to `<repo>/.github/workflows` and exists so the self-test can point it at fixtures.

**Design note — why not `yq`:** the spec suggested `bash` + `yq`. Rejected during planning: `yq` is not an established dependency of this repo (the script toolchain is `bash` + `jq`, per `scripts/check-doc-coverage.sh` and `scripts/apply-repo-config.sh`), its presence on the `ubuntu-latest` image could not be confirmed, and adding it means either an unpinned install step or a contributor-facing `brew install`. A line-oriented parser plus a self-test that pins its contract matches `check-advisory-ignore-sync.sh`, which already accepts a documented matching limitation for the same reason.

- [ ] **Step 1: Write the failing self-test**

Create `scripts/check-cargo-profile-env-sync-selftest.sh`:

```bash
#!/usr/bin/env bash
# check-cargo-profile-env-sync-selftest.sh — pin the contract of
# check-cargo-profile-env-sync.sh.
#
# That script is a line-oriented parser, not a YAML parser (see its header for
# why). This self-test is what makes that acceptable: it asserts the parser's
# behaviour against synthetic fixtures covering each way the real workflows are
# shaped, so a reformatting that breaks the parser fails here rather than
# silently reporting agreement in a required job.
#
# Run: bash scripts/check-cargo-profile-env-sync-selftest.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/check-cargo-profile-env-sync.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

failures=0

# Assert the script's exit status against a fixture directory.
#   expect <expected-status> <case-name> <fixture-dir>
expect() {
  local want="$1" name="$2" dir="$3" got=0
  bash "${script}" "${dir}" >"${tmp}/out.txt" 2>&1 || got=$?
  if [[ "${got}" == "${want}" ]]; then
    echo "ok    — ${name}"
  else
    echo "FAIL  — ${name}: expected exit ${want}, got ${got}" >&2
    sed 's/^/        /' "${tmp}/out.txt" >&2
    failures=$((failures + 1))
  fi
}

# A cache-bearing workflow with the given workflow-level env body and an
# optional extra block spliced into the job.
#   make_workflow <dir> <name> <env-body> [extra-job-yaml]
make_workflow() {
  local dir="$1" name="$2" env_body="$3" extra="${4:-}"
  mkdir -p "${dir}"
  {
    echo "name: ${name}"
    echo "on:"
    echo "  push:"
    echo "    branches: [main]"
    echo "env:"
    printf '%s\n' "${env_body}"
    echo ""
    echo "jobs:"
    echo "  build:"
    echo "    runs-on: ubuntu-latest"
    [[ -n "${extra}" ]] && printf '%s\n' "${extra}"
    echo "    steps:"
    echo "      # Swatinem/rust-cache v2.9.2"
    echo "      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6"
    echo "      - run: cargo build"
  } > "${dir}/${name}.yml"
}

# --- case 1: two cache-bearing workflows that agree ------------------------
d="${tmp}/agree"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always"
expect 0 "identical cargo-visible env passes" "${d}"

# --- case 2: one workflow missing the variable entirely -------------------
d="${tmp}/missing"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  NIGHTLY_TOOLCHAIN: nightly-2026-05-01"
expect 1 "a workflow missing a cargo-visible var fails" "${d}"

# --- case 3: same names, different values ---------------------------------
d="${tmp}/values"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: never"
expect 1 "a differing value fails" "${d}"

# --- case 4: non-matching prefixes are ignored ----------------------------
# NIGHTLY_TOOLCHAIN / HELIKON_* are not hashed by rust-cache, so they may
# differ freely. This guards against an over-broad prefix match.
d="${tmp}/ignored"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always
  NIGHTLY_TOOLCHAIN: nightly-2026-05-01"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always"
expect 0 "non-hashed variables may differ" "${d}"

# --- case 5: a workflow with no rust-cache step is out of scope -----------
d="${tmp}/uncached"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
mkdir -p "${d}"
cat > "${d}/uncached.yml" <<'YAML'
name: uncached
on:
  push:
    branches: [main]
env:
  CARGO_TERM_COLOR: never
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo no cache here
YAML
expect 0 "a workflow without rust-cache is not compared" "${d}"

# --- case 6: job-level cargo-visible env is rejected ----------------------
d="${tmp}/joblevel"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always" "    env:
      CARGO_INCREMENTAL: 0"
expect 1 "job-level cargo-visible env fails" "${d}"

# --- case 7: job-level env with a non-hashed name is allowed --------------
# ci.yml really does this: `test` sets HELIKON_REQUIRE_SANDBOX and
# `doc-coverage` sets DOC_COVERAGE_THRESHOLD at job level.
d="${tmp}/joblevel-ok"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always" "    env:
      HELIKON_REQUIRE_SANDBOX: 1"
expect 0 "job-level non-hashed env is allowed" "${d}"

# --- case 8: env on the rust-cache step itself is rejected ----------------
d="${tmp}/stepenv"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
mkdir -p "${d}"
cat > "${d}/beta.yml" <<'YAML'
name: beta
on:
  push:
    branches: [main]
env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      # Swatinem/rust-cache v2.9.2
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
        env:
          CARGO_INCREMENTAL: 0
      - run: cargo build
YAML
expect 1 "env on the rust-cache step itself fails" "${d}"

# --- case 9: step-level env on a LATER step is allowed --------------------
# ci.yml's `docs` job sets RUSTDOCFLAGS on its `cargo doc` step. That step runs
# after the cache step, so the variable is absent when the key is computed.
d="${tmp}/laterstep"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
mkdir -p "${d}"
cat > "${d}/beta.yml" <<'YAML'
name: beta
on:
  push:
    branches: [main]
env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      # Swatinem/rust-cache v2.9.2
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
      - run: cargo doc
        env:
          RUSTDOCFLAGS: "-D warnings"
YAML
expect 0 "step-level env on a later step is allowed" "${d}"

# --- case 10: the real workflows pass -------------------------------------
expect 0 "the repository's own workflows agree" "${repo_root}/.github/workflows"

echo
if [[ "${failures}" == "0" ]]; then
  echo "check-cargo-profile-env-sync selftest: all cases passed"
  exit 0
fi
echo "check-cargo-profile-env-sync selftest: ${failures} case(s) failed" >&2
exit 1
```

- [ ] **Step 2: Run the self-test to verify it fails**

```bash
bash scripts/check-cargo-profile-env-sync-selftest.sh
```

Expected: every case FAILs, because `scripts/check-cargo-profile-env-sync.sh`
does not exist yet (`bash: … No such file or directory`, exit 127 against
expectations of 0 and 1).

- [ ] **Step 3: Write the guard script**

Create `scripts/check-cargo-profile-env-sync.sh`:

```bash
#!/usr/bin/env bash
# check-cargo-profile-env-sync.sh — assert the cargo-visible environment is
# uniform across every workflow that uses Swatinem/rust-cache.
#
# SMA-618. rust-cache hashes every environment variable whose name begins with
# CARGO, CC, CFLAGS, CXX, CMAKE or RUST into the cache key, alongside the rustc
# version (src/config.ts at the pinned SHA). Two jobs meant to share a cache
# entry share it only while they declare byte-identical values for all of them.
#
# Drift fails nothing on its own: the keys simply diverge, an extra
# multi-gigabyte entry appears, the repository crosses GitHub's fixed and
# un-raisable 10 GB cache limit, LRU eviction resumes, and CI legs start running
# cold on a random rotation — the exact condition SMA-618 exists to remove,
# returning silently and with no red gate. This script is the red gate.
#
# Two assertions:
#
#   1. Every workflow containing a `Swatinem/rust-cache` step declares an
#      identical set of *workflow-level* `env:` entries whose names match the
#      hashed prefixes above.
#   2. No such workflow declares a matching variable at *job* level, and no
#      rust-cache step carries its own `env:`. Job-level env is in scope when
#      the cache step runs; step-level env on a *different* step is not, which
#      is why RUSTDOCFLAGS on ci.yml's `docs` job (set on the `cargo doc` step,
#      which runs after the cache step) is legitimate and is not flagged.
#
# Known limitation, accepted for the same reason check-advisory-ignore-sync.sh
# accepts its own: this is a line-oriented parser, not a YAML parser. It depends
# on the repository's existing workflow formatting — a top-level `env:` at
# column 0 with entries at two spaces, jobs at two spaces with `env:` at four,
# steps at six with `env:` at eight. Reformatting the workflows means updating
# this script with them. scripts/check-cargo-profile-env-sync-selftest.sh pins
# that contract; run it after any change here.
#
# Assertion 2 is deliberately conservative: it flags job-level matches in *any*
# job of a cache-bearing workflow, not only in jobs that themselves have a cache
# step. A false positive costs one comment; a false negative costs the budget.
#
# Escape hatch: a workflow that legitimately needs a divergent cargo-visible
# variable is listed in NON_SHARING below, with a comment saying why.
#
# Usage: bash scripts/check-cargo-profile-env-sync.sh [WORKFLOW_DIR]
#   exit 0 — agreement
#   exit 1 — drift (the failure this guards)
#   exit 2 — usage or environment error

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow_dir="${1:-${repo_root}/.github/workflows}"

# Workflows exempted from assertion 1. Empty today; add an entry with a reason
# rather than loosening the prefix list.
NON_SHARING=()

# The prefixes rust-cache hashes. Keep in step with src/config.ts at the pinned
# SHA; a prefix added upstream and missed here is a silent hole in the guard.
HASHED_PREFIX_RE='^(CARGO|CC|CFLAGS|CXX|CMAKE|RUST)'

if [[ ! -d "${workflow_dir}" ]]; then
  echo "error: workflow directory not found: ${workflow_dir}" >&2
  exit 2
fi

is_exempt() {
  local needle="$1" w
  for w in ${NON_SHARING[@]+"${NON_SHARING[@]}"}; do
    [[ "${w}" == "${needle}" ]] && return 0
  done
  return 1
}

# Emit the workflow-level env entries matching the hashed prefixes, sorted.
# The block runs from a column-0 `env:` to the next column-0 token.
workflow_env() {
  awk '
    /^env:[[:space:]]*$/ { inblock = 1; next }
    inblock && /^[^[:space:]#]/ { inblock = 0 }
    inblock && /^  [A-Za-z_][A-Za-z0-9_]*:/ { sub(/^  /, ""); print }
  ' "$1" | grep -E "${HASHED_PREFIX_RE}" | sort || true
}

# Emit job-level env entry NAMES matching the hashed prefixes (assertion 2a).
job_env_names() {
  awk '
    /^    env:[[:space:]]*$/ { inblock = 1; next }
    inblock && /^ {0,4}[^ #]/ { inblock = 0 }
    inblock && /^      [A-Za-z_][A-Za-z0-9_]*:/ {
      sub(/^      /, ""); sub(/:.*$/, ""); print
    }
  ' "$1" | grep -E "${HASHED_PREFIX_RE}" | sort -u || true
}

# Emit a marker line per rust-cache step that carries its own env: (2b).
cache_step_env() {
  awk '
    /Swatinem\/rust-cache@/ { instep = 1; next }
    instep && /^      - / { instep = 0 }
    instep && /^        env:[[:space:]]*$/ { print FILENAME ": rust-cache step has its own env:" }
  ' "$1" || true
}

mapfile -t all_workflows < <(find "${workflow_dir}" -maxdepth 1 -name '*.yml' | sort)
if [[ "${#all_workflows[@]}" == "0" ]]; then
  echo "error: no workflow files in ${workflow_dir}" >&2
  exit 2
fi

cached=()
for f in "${all_workflows[@]}"; do
  grep -q 'Swatinem/rust-cache@' "${f}" && cached+=("${f}")
done

if [[ "${#cached[@]}" == "0" ]]; then
  echo "error: no workflow uses Swatinem/rust-cache; this guard is misplaced" >&2
  exit 2
fi

problems=()

# --- assertion 1: identical workflow-level cargo-visible env ---------------
baseline=""
baseline_file=""
for f in "${cached[@]}"; do
  name="$(basename "${f}")"
  is_exempt "${name}" && continue
  env_block="$(workflow_env "${f}")"
  if [[ -z "${baseline_file}" ]]; then
    baseline="${env_block}"
    baseline_file="${name}"
    continue
  fi
  if [[ "${env_block}" != "${baseline}" ]]; then
    problems+=("${name}: workflow-level cargo-visible env differs from ${baseline_file}")
    problems+=("  ${baseline_file}: $(printf '%s' "${baseline}"  | tr '\n' ' ')")
    problems+=("  ${name}: $(printf '%s' "${env_block}" | tr '\n' ' ')")
  fi
done

# --- assertion 2: no job-level, and none on the cache step ----------------
for f in "${cached[@]}"; do
  name="$(basename "${f}")"
  while IFS= read -r var; do
    [[ -n "${var}" ]] || continue
    problems+=("${name}: job-level env '${var}' is inside the cache key; move it to the workflow-level env: block")
  done < <(job_env_names "${f}")
  while IFS= read -r hit; do
    [[ -n "${hit}" ]] || continue
    problems+=("${name}: a Swatinem/rust-cache step declares its own env:; it is hashed into that job's key alone")
  done < <(cache_step_env "${f}")
done

if [[ "${#problems[@]}" == "0" ]]; then
  echo "cargo-visible workflow env agrees across ${#cached[@]} cache-bearing workflow(s)"
  printf '%s' "${baseline}" | sed 's/^/  /'
  echo
  exit 0
fi

{
  echo "error: the cargo-visible environment has drifted between workflows"
  echo
  printf '  %s\n' "${problems[@]}"
  echo
  echo "These variable names are hashed into the rust-cache cache key, so drift"
  echo "silently splits a shared cache entry in two and pushes the repository"
  echo "back over GitHub's 10 GB limit. See docs/runbooks/ci-architecture.md"
  echo "-> Actions cache budget."
} >&2

exit 1
```

- [ ] **Step 4: Make both scripts executable**

```bash
chmod +x scripts/check-cargo-profile-env-sync.sh \
         scripts/check-cargo-profile-env-sync-selftest.sh
```

- [ ] **Step 5: Run the self-test — expect case 10 to fail**

```bash
bash scripts/check-cargo-profile-env-sync-selftest.sh
```

Expected: cases 1–9 pass; **case 10 fails** with exit 1, because `msrv.yml` and
`bench.yml` currently declare no workflow-level `env:` block at all while the
other five declare `CARGO_TERM_COLOR: always`. That is the real, pre-existing
drift this ticket found — Task 2 fixes it. Do not "fix" it by weakening the
script.

- [ ] **Step 6: Commit**

```bash
git add scripts/check-cargo-profile-env-sync.sh \
        scripts/check-cargo-profile-env-sync-selftest.sh
git commit -m "ci(workflows): SMA-618 add the cargo-visible env drift guard"
```

---

### Task 2: Normalize the workflow env and wire the guard into `fmt`

**Files:**
- Modify: `.github/workflows/msrv.yml` (add `env:` after the `permissions:` block)
- Modify: `.github/workflows/bench.yml` (add `env:` after the `permissions:` block)
- Modify: `.github/workflows/ci.yml` (`fmt` job, after the `cargo fmt` step)

**Interfaces:**
- Consumes: `scripts/check-cargo-profile-env-sync.sh` from Task 1.
- Produces: a green `bash scripts/check-cargo-profile-env-sync-selftest.sh` including case 10, and a `fmt` job that fails on future drift.

- [ ] **Step 1: Add the env block to `msrv.yml`**

`msrv.yml` currently goes straight from `permissions:` to `jobs:`. Insert
between them:

```yaml
# SMA-618: this block is not cosmetic. rust-cache hashes every CARGO*/RUST*
# variable into its cache key, so a workflow that omits one computes a
# different key and silently stops sharing a cache entry with the jobs it is
# meant to share with. Keep byte-identical with the other cache-bearing
# workflows; scripts/check-cargo-profile-env-sync.sh enforces it.
env:
  CARGO_TERM_COLOR: always
```

- [ ] **Step 2: Add the same block to `bench.yml`**

Insert the identical comment and `env:` block between `permissions:` and
`jobs:` in `.github/workflows/bench.yml`.

- [ ] **Step 3: Run the self-test to verify case 10 now passes**

```bash
bash scripts/check-cargo-profile-env-sync-selftest.sh
```

Expected: all ten cases pass, ending
`check-cargo-profile-env-sync selftest: all cases passed`.

- [ ] **Step 4: Add the guard step to the `fmt` job**

In `.github/workflows/ci.yml`, append to the `fmt` job's `steps:`, directly
after `- run: cargo fmt --all -- --check`:

```yaml
      # SMA-618: runs here because `fmt` is the cheapest job and the only one
      # with no rust-cache step of its own to perturb.
      - name: Check cargo-visible workflow env is in sync
        run: bash scripts/check-cargo-profile-env-sync.sh
```

- [ ] **Step 5: Verify the guard passes against the real tree**

```bash
bash scripts/check-cargo-profile-env-sync.sh
```

Expected: `cargo-visible workflow env agrees across 7 cache-bearing workflow(s)`
followed by an indented `CARGO_TERM_COLOR: always`.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/msrv.yml .github/workflows/bench.yml \
        .github/workflows/ci.yml
git commit -m "ci: SMA-618 normalize cargo-visible workflow env and gate it in fmt"
```

---

### Task 3: Restrict cache saving to `main`, and stop caching targets on `audit`/`deny`

Twelve sites. Every one gets the same `save-if` expression; `audit` and `deny`
additionally get `cache-targets: false`.

**Files:**
- Modify: `.github/workflows/ci.yml` (6 sites: `clippy`, `test`, `build-no-default-features`, `docs`, `doc-coverage`, `sessions-it`)
- Modify: `.github/workflows/msrv.yml` (1 site), `audit.yml` (1), `deny.yml` (1), `sbom.yml` (1), `bench.yml` (1), `integration.yml` (1)

**Interfaces:**
- Consumes: the normalized env from Task 2.
- Produces: no PR run writes a cache entry. PR runs still restore, because `save-if` gates only the action's `post` step (`dist/save.js`) and GitHub's cache scoping lets a `refs/pull/N/merge` ref read entries created on the default branch.

- [ ] **Step 1: Add `save-if` to the four bare `ci.yml` sites**

Four sites currently have no `with:` block at all — `clippy`, `test`,
`build-no-default-features`, `docs`. For each, the line
`      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6`
becomes:

```yaml
      # SMA-618: PR jobs restore but never save. save-if gates only the post
      # (save) step, so a PR run still reads main's entries — it just stops
      # writing its own refs/pull/N/merge copies, which is what was evicting
      # main's and pushing the repo 37% over the fixed 10 GB limit.
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
```

Write the full SMA-618 comment once, at the **first** site (`clippy`). At the
other three, use the short form so the file does not repeat four paragraphs:

```yaml
      # SMA-618: save only from the default branch — see the note on `clippy`.
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
```

Keep the existing `# Swatinem/rust-cache v2.9.2` version comment above each.

- [ ] **Step 2: Add `save-if` to the two indented `ci.yml` sites**

`doc-coverage` uses the same six-space form as above. `sessions-it` is
different — it is `if:`-gated, so its step is already a mapping:

```yaml
      # Swatinem/rust-cache v2.9.2
      # SMA-618: save only from the default branch — see the note on `clippy`.
      - if: steps.filter.outputs.sessions == 'true'
        uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
```

- [ ] **Step 3: Verify all six `ci.yml` sites carry the expression**

```bash
grep -c "save-if: \${{ github.ref == 'refs/heads/main' }}" .github/workflows/ci.yml
```

Expected: `6`

- [ ] **Step 4: Add `save-if` to `msrv.yml`, `sbom.yml`, `bench.yml`, `integration.yml`**

`msrv.yml`, `sbom.yml` and `bench.yml` have bare sites — use the short-comment
form from Step 1. `integration.yml` already has a `with:` block; add the key to
it, keeping `cache-on-failure`:

```yaml
      # Swatinem/rust-cache v2.9.2 — cache-on-failure because this job is expected
      # to flake while it earns promotion.
      # SMA-618: save only from the default branch.
      - if: steps.decide.outputs.run == 'true'
        uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
        with:
          cache-on-failure: "true"
          save-if: ${{ github.ref == 'refs/heads/main' }}
```

For `sbom.yml`, add this note above its site — it is the one place where the
expression is never true, and a future reader will otherwise think it is a bug:

```yaml
      # SMA-618: sbom.yml is tag-triggered only, so this expression never holds
      # and the step becomes restore-only. That is intended: main never runs
      # `sbom`, so there is no sbom-keyed entry to restore either, and the step
      # is inert until PR 2 makes it a registry reader. Do not "fix" it to save
      # on tags — that would spend budget on the release path this ticket frees.
```

- [ ] **Step 5: Add `save-if` and `cache-targets: false` to `audit.yml` and `deny.yml`**

Both already have a `with:` block carrying `cache-directories`. `audit.yml`:

```yaml
      # Swatinem/rust-cache v2.9.2
      # SMA-618: cargo-audit never compiles the workspace — it reads Cargo.lock
      # and the advisory DB — so a target cache here is pure waste against a
      # fixed 10 GB budget. cache-targets: false keeps ~/.cargo (registry) and
      # the cache-directories entry below, and drops only target/.
      # One-time cost: @actions/cache folds the cached path list into the cache
      # *version*, so the existing entry becomes unreachable and this job runs
      # cold once.
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
        with:
          cache-directories: "~/.cargo/advisory-db"
          cache-targets: "false"
          save-if: ${{ github.ref == 'refs/heads/main' }}
```

`deny.yml` is identical except `cache-directories: "~/.cargo/advisory-dbs"`
(note the plural — do not normalize it; the two tools use different paths) and a
first line naming `cargo-deny` rather than `cargo-audit`.

- [ ] **Step 6: Verify all twelve sites**

```bash
grep -rc "save-if: \${{ github.ref == 'refs/heads/main' }}" .github/workflows/ \
  | grep -v ':0$'
```

Expected exactly:

```text
.github/workflows/audit.yml:1
.github/workflows/bench.yml:1
.github/workflows/ci.yml:6
.github/workflows/deny.yml:1
.github/workflows/integration.yml:1
.github/workflows/msrv.yml:1
.github/workflows/sbom.yml:1
```

Total 12. Then confirm no site was missed:

```bash
grep -rc "Swatinem/rust-cache@" .github/workflows/ | grep -v ':0$'
```

Expected: the same seven files with the same counts.

- [ ] **Step 7: Re-run the guard — the env must still agree**

```bash
bash scripts/check-cargo-profile-env-sync.sh \
  && bash scripts/check-cargo-profile-env-sync-selftest.sh
```

Expected: both pass. (Assertion 2 covers `with:` blocks only incidentally, but
this catches an accidental `env:` typed into a cache step.)

- [ ] **Step 8: Commit**

```bash
git add .github/workflows/
git commit -m "ci: SMA-618 save rust caches only from the default branch"
```

---

### Task 4: Daily cache budget monitor

**Files:**
- Create: `.github/workflows/cache-budget.yml`

**Interfaces:**
- Consumes: nothing.
- Produces: a schedule-only workflow emitting a `::warning::` when total cache usage exceeds 8.5 GiB, plus a step summary table. Never fails.

**Design note — why a new workflow, not a job in `audit.yml`:** the spec said to
add a step to `audit.yml`'s existing daily cron. Rejected during planning.
`CLAUDE.md` instructs readers to take the audit **run** conclusion as the
supply-chain verdict ("read the *run* conclusion, never that job's status"). Any
additional job in `audit.yml` can turn a green audit into a red run for a reason
that has nothing to do with security — a transient `gh api` failure would
corrupt exactly the signal that document tells people to trust. A separate
workflow keeps it clean, and costs one file.

- [ ] **Step 1: Create the workflow**

```yaml
name: cache-budget

# SMA-618. GitHub's Actions cache limit is 10 GB per repository and is not
# raisable. This repository sat 37% over it for months with nothing red: every
# push evicted somebody's entry, and a different CI leg paid a cold build each
# run — once measured at 42m40s for `test (windows-latest, stable)`. Nothing in
# CI reported it, because going over the limit is not an error, it is just
# eviction.
#
# This workflow is what makes that budget an enforceable standing constraint
# rather than an aspiration.
#
# Deliberately NOT a job inside audit.yml, even though audit.yml already has a
# daily cron on main. CLAUDE.md instructs readers to take the audit *run*
# conclusion as the supply-chain verdict, so an unrelated job in that workflow
# could turn a green audit into a red run — corrupting the signal that document
# tells people to trust. Keep this separate.
#
# Warns, never fails. A budget drifting toward the limit is something to fix
# deliberately, not something to block a merge on.

on:
  schedule:
    - cron: "43 6 * * *"   # daily — offset from audit.yml (06:00) and deny.yml (06:17)
  workflow_dispatch:

concurrency:
  group: cache-budget-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: false

permissions:
  contents: read
  actions: read   # required to list the repository's Actions caches

jobs:
  report:
    runs-on: ubuntu-latest
    steps:
      - name: Report Actions cache usage
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # 8.5 GiB. Warns with headroom, so the first notice arrives before
          # eviction starts rather than after it has already gone silent.
          WARN_BYTES: "9126805504"
        run: |
          set -euo pipefail

          # The LIST endpoint, never actions/cache/usage. SMA-618 measured the
          # latter reporting active_caches_count: 4 while the list returned six
          # rows; the byte totals agreed, so the count field is unreliable and
          # the list is the source of truth.
          #
          # Guarded rather than a bare assignment: under `set -e`, an
          # unguarded `rows="$(gh api ...)"` would abort the script on any
          # `gh api` failure (rate limit, transient 5xx, denied `actions:
          # read`, network blip) and redden this run — defeating the
          # warns-never-fails contract this workflow exists to keep.
          if ! rows="$(gh api --paginate "repos/${GITHUB_REPOSITORY}/actions/caches" \
            --jq '.actions_caches[] | [.size_in_bytes, .ref, .key] | @tsv')"; then
            echo "::warning::Could not query Actions cache usage via gh api; skipping this run's report."
            exit 0
          fi

          total=0
          count=0
          {
            echo "### Actions cache inventory"
            echo
            echo "| Size | Ref | Key |"
            echo "| -- | -- | -- |"
          } >> "${GITHUB_STEP_SUMMARY}"

          while IFS=$'\t' read -r size ref key; do
            [ -n "${size}" ] || continue
            total=$((total + size))
            count=$((count + 1))
            human="$(numfmt --to=iec-i --suffix=B "${size}")"
            printf '%10s  %-24s %s\n' "${human}" "${ref}" "${key}"
            printf '| %s | `%s` | `%s` |\n' "${human}" "${ref}" "${key}" \
              >> "${GITHUB_STEP_SUMMARY}"
          done <<< "${rows}"

          human_total="$(numfmt --to=iec-i --suffix=B "${total}")"
          echo
          echo "total: ${human_total} across ${count} entries"
          {
            echo
            echo "**Total: ${human_total} across ${count} entries** (limit: 10 GB)"
          } >> "${GITHUB_STEP_SUMMARY}"

          if [ "${total}" -gt "${WARN_BYTES}" ]; then
            echo "::warning::Actions cache usage is ${human_total}, above the $(numfmt --to=iec-i --suffix=B "${WARN_BYTES}") warning line. GitHub's hard limit is 10 GB and eviction is silent. See docs/runbooks/ci-architecture.md -> Actions cache budget."
          fi
```

- [ ] **Step 2: Verify the workflow parses and the guard still passes**

The guard only inspects cache-bearing workflows, and this one has no
`rust-cache` step, so it must remain out of scope:

```bash
bash scripts/check-cargo-profile-env-sync.sh
grep -c "Swatinem/rust-cache" .github/workflows/cache-budget.yml || true
```

Expected: the guard still reports agreement across **7** workflows (not 8), and
the `grep -c` prints `0`.

- [ ] **Step 3: Dry-run the reporting logic locally**

The script body is plain bash plus `gh`; run its core against the live repo to
confirm the jq expression and `numfmt` handling:

```bash
gh api --paginate repos/SMK1085/paigasus-helikon/actions/caches \
  --jq '.actions_caches[] | [.size_in_bytes, .ref, .key] | @tsv' \
| awk -F'\t' '{t+=$1; n++; printf "%12d  %-24s %s\n", $1, $2, $3} END {printf "total %d bytes across %d entries\n", t, n}'
```

Expected: a table plus a total. As of 2026-09-06 that total is ~13.7 GB across
6 entries — i.e. the warning line would fire, which is correct.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/cache-budget.yml
git commit -m "ci(workflows): SMA-618 report actions cache usage daily"
```

---

### Task 5: Documentation

**Files:**
- Modify: `CLAUDE.md` (CI section — the local-reproduction list near line 28, and the CI prose near line 91)
- Modify: `CONTRIBUTING.md` (the local-gate list near line 239)
- Modify: `docs/runbooks/ci-architecture.md` (new section; plus the false `actionlint`/`shellcheck` claim on line 31)

**Interfaces:**
- Consumes: everything from Tasks 1–4.
- Produces: no code interface. This is the record that keeps the next contributor from undoing the fix.

- [ ] **Step 1: Fix the false claim in `docs/runbooks/ci-architecture.md`**

Line 31 currently reads, inside the protoc section:

> `actionlint` lints `.github/workflows/*.yml` and **not** `action.yml`, so `shellcheck` on the three scripts is the only lint coverage the install logic has.

Neither tool runs anywhere in this repository — verified: the only match in the
whole tree is a `# shellcheck source=/dev/null` directive comment inside
`.github/actions/setup-protoc/selftest.sh`. Replace with:

> Neither `actionlint` nor `shellcheck` runs in CI (SMA-618 checked: the only match in the tree is a `# shellcheck` directive comment inside `selftest.sh`), so `selftest.sh` is the **only** coverage the install logic has — which is why it re-downloads every published asset rather than merely asserting the digests are well-formed.

- [ ] **Step 2: Add the "Actions cache budget" section to the runbook**

Append a new `## Actions cache budget` section covering: the fixed 10 GB limit;
that PR jobs no longer save; why `save-if` still leaves restore working; the
`cache-targets: false` rationale for `audit`/`deny`; the cargo-visible env rule
and the guard script; the `cache-budget.yml` monitor; the measurement procedure
(**list** endpoint, never `actions/cache/usage` — `active_caches_count` was
measured reporting 4 against a six-row list); and the purge command with its
`actions: write` requirement and the note that it is not one-time while
un-rebased PRs exist. Reference the spec for the full derivation.

- [ ] **Step 3: Update `CLAUDE.md`**

Add `bash scripts/check-cargo-profile-env-sync.sh` to the local-reproduction
command block (after the `check-markdownlint-config.sh` line), and add a short
paragraph to the CI section stating: the 10 GB cache limit is a standing
constraint monitored daily by `cache-budget.yml`; caches are saved **only** from
`main`; the `CARGO_*`/`RUST*` workflow env is inside the cache key and must stay
uniform across cache-bearing workflows, enforced by the new script in `fmt`; and
that `audit`/`deny` deliberately cache no target directory. Note that
`ci.yml` now runs **ten** jobs on a PR, not nine — `fmt` gained a step, not a
job, so the job count is unchanged; verify before editing that sentence and
leave it alone if still accurate.

- [ ] **Step 4: Update `CONTRIBUTING.md`**

Add to the local-gate block after the `check-markdownlint-config.sh` line:

```bash
bash scripts/check-cargo-profile-env-sync.sh   # cargo-visible workflow env is uniform
```

- [ ] **Step 5: Lint the Markdown**

```bash
export PATH="$HOME/.nvm/versions/node/v24.20.0/bin:$PATH"
npx markdownlint-cli2 && bash scripts/check-markdownlint-config.sh
```

Expected: `Summary: 0 issues in 0 files`, then the config assertion passes. The
default `node` on this host is v18 and crashes `markdownlint-cli2` with
`SyntaxError: Invalid regular expression flags` — use the v24 path.

- [ ] **Step 6: Run the full local gate set**

```bash
cargo fmt --all -- --check
bash scripts/check-cargo-profile-env-sync.sh
bash scripts/check-cargo-profile-env-sync-selftest.sh
```

Expected: all pass. `cargo clippy` and `cargo test` are unaffected by this PR
(no Rust changed), but run `cargo clippy --workspace --all-features --all-targets
-- -D warnings` once anyway — the `pre-push` hook will run it regardless.

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md CONTRIBUTING.md docs/runbooks/ci-architecture.md
git commit -m "docs(ci): SMA-618 record the actions cache budget and its guards"
```

---

## Post-merge operational steps

Not code. These belong in the PR description and are executed by a human after
merge, **in this order** — the ordering is load-bearing.

1. **Purge every existing cache entry, before measuring anything.** Leaving the
   ~9.5 GB of stale PR-scoped entries alongside fresh ones means GitHub evicts
   the new ones as fast as they are written, and the measurement below reports a
   thrashing system — a false negative caused by the cleanup, not the design.

   ```bash
   gh api --paginate repos/SMK1085/paigasus-helikon/actions/caches \
     --jq '.actions_caches[].id' \
   | xargs -I{} gh api --method DELETE \
       repos/SMK1085/paigasus-helikon/actions/caches/{}
   ```

   Requires `actions: write`. No workflow performs this purge — it runs from a
   developer machine with a PAT carrying that scope; the workflow
   `GITHUB_TOKEN` is not involved. **Not one-time:** every PR still based on
   pre-merge `main` runs the *old* workflow definitions on its next push and
   keeps writing `refs/pull/N/merge` entries until rebased. Re-purge, or wait for
   #240 and #241 to rebase or merge.

2. Let one push-to-`main` run complete across `ci.yml`, `msrv.yml`, `audit.yml`,
   `deny.yml`, `integration.yml`.

3. Record the inventory:

   ```bash
   gh api repos/SMK1085/paigasus-helikon/actions/caches --paginate \
     --jq '.actions_caches[] | "\(.size_in_bytes) \(.ref) \(.key)"'
   ```

4. **Assert:** no entry exists under any `refs/pull/*` ref created after the
   merge, and `main` holds 15 entries. Caveat: `sessions-it` (`ci.yml`) and
   `temporal-it` (`integration.yml`) gate their cache steps behind path
   filters, and this merge push touches both `ci.yml` and `integration.yml`, so
   both filters fire and this specific run genuinely sees 15 — every later
   `main` push that does not touch those paths shows 13. Do not read 13 on a
   later measurement as a regression.

5. **Record every individual size in the spec**, replacing the `†`-marked
   inferred figures in its budget table. This measured baseline is what PR 2's
   budget is re-derived from, and PR 2's first task depends on it.

## Self-review

**Spec coverage.** Change A → Task 3. Change D (`audit`/`deny` `cache-targets`)
→ Task 3 Step 5; D's `sbom`/`bench` reader treatment is PR 2, with PR 1 leaving
`sbom` inert and documented (Task 3 Step 4). Change F assertions 1–2 → Tasks 1
and 2. Change G → Task 4. Docs → Task 5. Purge + measurement → post-merge steps.
Changes B, C, E and assertion 3 are PR 2 by design and are named in Global
Constraints so an executor does not reach for them.

**Two deliberate deviations from the spec**, both recorded inline with reasons:
`yq` → line-oriented bash plus a self-test (Task 1); budget monitor as its own
workflow rather than a job in `audit.yml`, because that workflow's run
conclusion is the documented audit verdict (Task 4).

**Known pre-existing condition, not a plan defect:** Task 1 Step 5 expects the
self-test's case 10 to fail before Task 2 runs. That is the real drift the
ticket found (`msrv.yml` and `bench.yml` have no `env:` block), and the plan
fixes it rather than weakening the guard around it.

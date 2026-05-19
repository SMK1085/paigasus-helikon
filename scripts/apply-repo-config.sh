#!/bin/sh
# Apply repo-level configuration (rulesets + merge settings) idempotently.
#
# Usage:
#   gh auth login              # one-time; needs `repo` scope minimum
#   sh scripts/apply-repo-config.sh
#
# What this script does:
#   1. Preflight: ensure `gh` is authenticated and `jq` is installed.
#   2. Resolve dependabot's GitHub App ID via the public /apps/dependabot
#      endpoint. (The release-plz workflow on this repo uses a private
#      App owned by the maintainer — its ID is hardcoded in
#      .github/rulesets/branch-names.json since the public /apps/{slug}
#      endpoint cannot resolve private Apps. See CONTRIBUTING.md
#      "Repo configuration" for the rationale.)
#   3. For each .github/rulesets/*.json, substitute the dependabot
#      placeholder and POST (create) or PUT (update) via the rulesets API.
#   4. Apply merge-method and squash-format settings via `gh repo edit`.
#
# Idempotent: re-running converges to the same state.

set -eu

REPO="SMK1085/paigasus-helikon"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RULESET_DIR="$SCRIPT_DIR/../.github/rulesets"

# ---------- 1. Preflight ----------

if ! gh auth status >/dev/null 2>&1; then
    echo "ERROR: 'gh' is not authenticated. Run 'gh auth login' first." >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: 'jq' is required but not installed." >&2
    echo "       macOS:  brew install jq" >&2
    echo "       Linux:  apt-get install jq  (or your distro equivalent)" >&2
    exit 1
fi

# ---------- 2. Resolve App IDs ----------

DEPENDABOT_APP_ID="$(gh api /apps/dependabot --jq .id)"
if [ -z "$DEPENDABOT_APP_ID" ]; then
    echo "ERROR: Could not resolve dependabot App ID via /apps/dependabot." >&2
    exit 1
fi

echo "Resolved App IDs: dependabot=$DEPENDABOT_APP_ID"

# ---------- 3. Apply rulesets ----------

EXISTING_RULESETS_JSON="$(gh api "repos/$REPO/rulesets")"

tmp_file="$(mktemp)"
trap 'rm -f "$tmp_file"' EXIT INT TERM

RULESET_COUNT=0
for ruleset_file in "$RULESET_DIR"/*.json; do
    RULESET_COUNT=$((RULESET_COUNT + 1))
    name="$(jq -r '.name' < "$ruleset_file")"
    # Substitute the dependabot placeholder (a quoted string token) with
    # the resolved bare numeric ID. The quoted-token approach keeps the
    # committed JSON parseable while still producing a numerically-typed
    # actor_id post-substitution. The paigasusbot App ID is already
    # hardcoded in branch-names.json (private App, can't be resolved
    # at apply time).
    sed \
        -e "s/\"DEPENDABOT_APP_ID\"/$DEPENDABOT_APP_ID/g" \
        "$ruleset_file" > "$tmp_file"

    existing_id="$(printf '%s' "$EXISTING_RULESETS_JSON" \
        | jq -r --arg name "$name" '.[] | select(.name == $name) | .id' \
        | head -1)"

    if [ -z "$existing_id" ]; then
        gh api -X POST "repos/$REPO/rulesets" --input "$tmp_file" >/dev/null
        printf '  %-30s created\n' "$name"
    else
        gh api -X PUT "repos/$REPO/rulesets/$existing_id" --input "$tmp_file" >/dev/null
        printf '  %-30s updated\n' "$name"
    fi
done

# ---------- 4. Apply merge settings ----------
#
# `gh repo edit`'s boolean toggles use --enable-X (no --enable-X=false syntax;
# the flag is a parameterless toggle and `=false` is silently dropped), and the
# CLI does not expose `--squash-merge-commit-title` at all. Talk to the
# underlying PATCH endpoint directly to avoid both quirks. `-F` for booleans
# (gh auto-types), `-f` for string enums.

gh api -X PATCH "repos/$REPO" \
    -F allow_merge_commit=false \
    -F allow_rebase_merge=false \
    -F allow_squash_merge=true \
    -F delete_branch_on_merge=true \
    -f squash_merge_commit_title=PR_TITLE \
    -f squash_merge_commit_message=BLANK >/dev/null

echo "Applied $RULESET_COUNT rulesets, repo settings updated."

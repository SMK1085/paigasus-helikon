#!/bin/sh
# Apply repo-level configuration (rulesets + merge settings) idempotently.
#
# Usage:
#   gh auth login              # one-time; needs `repo` scope minimum
#   bash scripts/apply-repo-config.sh
#
# What this script does:
#   1. Preflight: ensure `gh` is authenticated and `jq` is installed.
#   2. Resolve dependabot + release-plz GitHub App IDs via the public
#      /apps/{slug} endpoint (no installation check — App IDs are global
#      constants and the ruleset accepts them regardless of install
#      status).
#   3. For each .github/rulesets/*.json, substitute App ID placeholders
#      and POST (create) or PUT (update) via the rulesets API.
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

RELEASE_PLZ_APP_ID="$(gh api /apps/release-plz --jq .id)"
if [ -z "$RELEASE_PLZ_APP_ID" ]; then
    echo "ERROR: Could not resolve release-plz App ID via /apps/release-plz." >&2
    exit 1
fi

echo "Resolved App IDs: dependabot=$DEPENDABOT_APP_ID release-plz=$RELEASE_PLZ_APP_ID"

# ---------- 3. Apply rulesets ----------

EXISTING_RULESETS_JSON="$(gh api "repos/$REPO/rulesets")"

RULESET_COUNT=0
for ruleset_file in "$RULESET_DIR"/*.json; do
    RULESET_COUNT=$((RULESET_COUNT + 1))
    name="$(jq -r '.name' < "$ruleset_file")"
    tmp_file="$(mktemp)"
    # Substitute the literal quoted placeholders with bare numerics.
    # The quoted-token approach keeps the committed JSON parseable
    # while still producing a numerically-typed actor_id post-substitution.
    sed \
        -e "s/\"DEPENDABOT_APP_ID\"/$DEPENDABOT_APP_ID/g" \
        -e "s/\"RELEASE_PLZ_APP_ID\"/$RELEASE_PLZ_APP_ID/g" \
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
    rm -f "$tmp_file"
done

# ---------- 4. Apply merge settings ----------

gh repo edit "$REPO" \
    --enable-merge-commit=false \
    --enable-rebase-merge=false \
    --enable-squash-merge=true \
    --delete-branch-on-merge=true \
    --squash-merge-commit-title=PR_TITLE \
    --squash-merge-commit-message=BLANK

echo "Applied $RULESET_COUNT rulesets, repo settings updated."

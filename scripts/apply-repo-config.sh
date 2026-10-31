#!/bin/sh
# Apply repo-level configuration (rulesets, merge settings and the release
# environment) idempotently.
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
#   4. Apply merge-method and squash-format settings via a direct
#      `gh api -X PATCH /repos/{owner}/{repo}` call. (Not `gh repo edit`:
#      its `--enable-X=false` toggles are silently dropped, and it doesn't
#      expose `--squash-merge-commit-title` at all.)
#   5. Create or update the `release` environment. Only the `main` branch
#      can deploy to it (one custom branch policy). It has no required
#      reviewers and no wait timer, so the release stays automatic. The
#      environment secrets are not set here: set them by hand (see
#      docs/runbooks/release-ascend.md).
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

# ---------- 5. Apply the release environment ----------
#
# The release-plz job uses `environment: release`. The environment holds the
# release secrets, and its deployment branch policy allows only `main`. Thus a
# workflow on a different branch cannot read the secrets.
#
# This section does not set the secrets. GitHub does not return secret values,
# so this script cannot compare them. Set the secrets on the environment by
# hand. See docs/runbooks/release-ascend.md.
#
# The PUT does not set `reviewers` or `wait_timer`. The release must stay
# automatic.

ENV_NAME="release"

printf '%s\n' '{"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}' > "$tmp_file"
gh api -X PUT "repos/$REPO/environments/$ENV_NAME" --input "$tmp_file" >/dev/null

POLICIES_JSON="$(gh api "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies")"

# Delete each branch policy that is not the `main` branch policy, so that a
# re-run converges to exactly one policy.
printf '%s' "$POLICIES_JSON" \
    | jq -r '.branch_policies[] | select(.name != "main" or (.type // "branch") != "branch") | .id' \
    | while read -r policy_id; do
        [ -n "$policy_id" ] || continue
        gh api -X DELETE "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies/$policy_id" >/dev/null
        printf '  %-30s deleted branch policy %s\n' "$ENV_NAME" "$policy_id"
    done

main_policy_count="$(printf '%s' "$POLICIES_JSON" \
    | jq '[.branch_policies[] | select(.name == "main" and (.type // "branch") == "branch")] | length')"

if [ "$main_policy_count" -eq 0 ]; then
    gh api -X POST "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies" \
        -f name=main -f type=branch >/dev/null
    printf '  %-30s added branch policy main\n' "$ENV_NAME"
else
    printf '  %-30s branch policy main is present\n' "$ENV_NAME"
fi

echo "Applied $RULESET_COUNT rulesets, repo settings updated, environment '$ENV_NAME' updated."

# GitHub Actions Pinning Runbook

> **Scope:** how to resolve and pin a GitHub Action to a commit SHA, and why
> `dtolnay/rust-toolchain` needs its bump direction checked rather than assumed (SMA-486).
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling. It holds the *rationale and incident
> history*; the operative rules stay in `CLAUDE.md`.

## Resolving the latest release and its SHA

**Always implement GitHub Actions against the latest stable major.** Before adding or updating any `uses:` line in `.github/workflows/`, resolve the latest release of the action and pin to its commit SHA (never a moving `@vN` tag). Use:

```bash
gh api repos/<owner>/<repo>/releases/latest | jq -r '.tag_name'
gh api repos/<owner>/<repo>/git/ref/tags/<tag> | jq -r '.object.sha'
# if .object.type == "tag" (annotated), dereference:
# gh api repos/<owner>/<repo>/git/tags/<sha> | jq -r '.object.sha'
```

Do not use a plan-time version pin if a newer major has shipped between plan-writing and implementation — bump immediately, then let Dependabot's `gh-actions` group track updates from there. The human-readable version stays as a trailing `# vX.Y.Z` comment on the `uses:` line so the SHA is auditable (see "Version comments" below). The `gh-actions` group in `.github/dependabot.yml` sets `patterns: ["*"]` and no `update-types`, so its configuration permits major bumps too. It has had this form since SMA-308 (2026-05-17); the earlier `actions-minor-and-patch` group was patch + minor only. Do not depend on the configuration alone: `actions/checkout` sat on v6.0.2 until SMA-486 bumped it by hand, and the cause is not recorded here. Thus check for a new major by hand.

## Version comments

Put the version on the `uses:` line, as a trailing comment that **ends with** the version:

```yaml
      # Notes about the step stay on their own lines above it.
      - uses: taiki-e/install-action@5488ac5a951645ce95062214902ac4b6bc1a1037 # v2.87.32
```

Do not put the version on a separate comment line above the step, and do not write more text after the version. Dependabot then cannot update the comment (SMA-744).

The reason is how dependabot-core changes a comment. `WorkflowUpdater` gives `VersionCommenter#updated_comment` only the comment that is on the same line as the `uses:` declaration. `VersionCommenter` changes that comment only if it ends with a version tag of the old SHA, and then replaces that version with the most specific tag of the new SHA. Any other comment stays unchanged.

Before SMA-744, every workflow had the version on a separate line above the step (`# taiki-e/install-action v2.87.29`). Each Dependabot gh-actions group PR changed the SHAs and left every comment stale. The `branch-names` ruleset blocks pushes to `dependabot/**`, so each PR needed a `feature/**` takeover PR that corrected the comments: #302 → #303, #313 → #315 and #316 → #318.

To check that each comment agrees with its SHA, compare the pin with the tag. `git ls-remote` also shows annotated tags (`^{}`) and is not limited to the first 100 tags:

```bash
grep -hoE "uses: [^ ]+@[0-9a-f]{40} # v[0-9.]+" .github/workflows/*.yml \
  | sed -E 's/uses: //; s/ # / /' | sort -u \
  | while read -r ref ver; do
      repo=${ref%@*}; sha=${ref#*@}
      got=$(git ls-remote --tags "https://github.com/$repo.git" "refs/tags/$ver" "refs/tags/$ver^{}" | awk '{print $1}' | tail -1)
      [ "$got" = "$sha" ] && echo "OK $repo $ver" || echo "MISMATCH $repo $ver"
    done
```

`dtolnay/rust-toolchain` has no version comment, because its pin is a `master` commit, not a tag (see the next section).

## dtolnay/rust-toolchain — check the direction

**`dtolnay/rust-toolchain` needs its bump direction checked, not assumed.** It does not ship conventional releases — it publishes rolling *branches* (`stable`, `nightly`, `master`, `1.0`…`1.14`) and a `v1` tag that is re-pointed at intervals. As of 2026-08-18 `releases/latest`, the `v1` tag, `master`'s head, and this repo's pin are **all** `6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772`, so a Dependabot bump here is legitimate and was accepted in #201. That has not always been true: through SMA-486 `v1` sat abandoned on a 2025-08-23 commit **11 commits behind** the pin, and running the "bump to the latest release" recipe would have proposed an ~11-month **downgrade**. So do not treat this action as blanket-exempt *or* as blanket-safe — before accepting any bump, confirm the direction:

```bash
gh api repos/dtolnay/rust-toolchain/compare/<current-pin>...<proposed-sha> \
  --jq '{status, ahead_by, behind_by}'   # must be ahead_by > 0, behind_by == 0
```

Independently of all that, one rule is permanent: every site pins a SHA and **must** pair it with an explicit `with: toolchain: …`. The ref name is the toolchain selector, so a SHA ref carries no toolchain — the input is mandatory, not stylistic. The workflow comments point here.

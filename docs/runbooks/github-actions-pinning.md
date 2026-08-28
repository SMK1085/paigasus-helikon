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

Do not use a plan-time version pin if a newer major has shipped between plan-writing and implementation — bump immediately, then let Dependabot's `github-actions` group track patch/minor updates from there. The above-the-fold human-readable version stays as a `# action vX.Y.Z` comment so the SHA is auditable. Note the corollary: because that group is configured for **patch + minor only**, a major bump never arrives on its own and must be swept by hand — which is how `actions/checkout` sat on v6.0.2 until SMA-486.

## dtolnay/rust-toolchain — check the direction

**`dtolnay/rust-toolchain` needs its bump direction checked, not assumed.** It does not ship conventional releases — it publishes rolling *branches* (`stable`, `nightly`, `master`, `1.0`…`1.14`) and a `v1` tag that is re-pointed at intervals. As of 2026-08-18 `releases/latest`, the `v1` tag, `master`'s head, and this repo's pin are **all** `6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772`, so a Dependabot bump here is legitimate and was accepted in #201. That has not always been true: through SMA-486 `v1` sat abandoned on a 2025-08-23 commit **11 commits behind** the pin, and running the "bump to the latest release" recipe would have proposed an ~11-month **downgrade**. So do not treat this action as blanket-exempt *or* as blanket-safe — before accepting any bump, confirm the direction:

```bash
gh api repos/dtolnay/rust-toolchain/compare/<current-pin>...<proposed-sha> \
  --jq '{status, ahead_by, behind_by}'   # must be ahead_by > 0, behind_by == 0
```

Independently of all that, one rule is permanent: every site pins a SHA and **must** pair it with an explicit `with: toolchain: …`. The ref name is the toolchain selector, so a SHA ref carries no toolchain — the input is mandatory, not stylistic. The workflow comments point here.

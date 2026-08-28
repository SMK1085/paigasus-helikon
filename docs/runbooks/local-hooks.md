# Local Git Hooks Reference

> **Scope:** cargo-husky hook installation mechanics, the git-worktree silent-failure
> mode, hook-manager chaining, and the convco merge-base fix (SMA-547).
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling. It holds the *rationale and incident
> history*; the operative rules stay in `CLAUDE.md`.

## Installation mechanics

Hooks are managed via `cargo-husky` (user-hooks mode) and live in `.cargo-husky/hooks/`. They're installed into `.git/hooks/` on the next dev-dep realization of `paigasus-helikon` (e.g. `cargo test -p paigasus-helikon --no-run`). To force re-install after editing a hook: `rm -rf target/debug/build/cargo-husky-* && cargo test -p paigasus-helikon --no-run`.

## The worktree silent failure

**That re-install silently does nothing inside a git worktree.** cargo-husky walks up from its `OUT_DIR` looking for a `.git` **directory**; in a worktree `.git` is a *file* pointing at `…/.git/worktrees/<name>`, so the search fails and the build script exits having installed nothing. The only trace is a `Warning: .git directory was not found in …` line in `target/debug/build/cargo-husky-*/stderr`, which cargo does not surface — the build looks clean and the old hook keeps running. Worktrees share the main checkout's `.git/hooks/` (`git rev-parse --git-common-dir`), so after editing a hook from a worktree, run the re-install from the **main checkout**. Verify with `grep` against the installed copy rather than assuming — a stale `pre-push` survived an entire branch's worth of pushes in SMA-547 this way.

## Do not hand-copy a hook

**Do not hand-copy a hook onto `.git/hooks/<name>`.** On this machine the Entire CLI owns `.git/hooks/pre-push`: its wrapper pushes session logs, then chains to the previous hook at `.git/hooks/pre-push.pre-entire`, which is where the cargo-husky hook actually lives. Overwriting `pre-push` silently destroys the Entire integration while still appearing to work, because the cargo-husky body runs either way. Check for a `.pre-entire` sibling first and install into that slot, preserving cargo-husky's three-line `# This hook was set by cargo-husky` banner. Any hook manager that chains this way (Entire, lefthook, pre-commit) has the same shape.

## The convco baseline must be a merge-base

**The convco baseline must be a merge-base, not a branch tip** (fixed in SMA-547). `convco check A..B` only walks the commits git would list when `A` is an **ancestor** of `B`; given a diverged `A` it silently falls back to the entire history instead — here 220 commits back to `Initial commit`, three of which predate the `.versionrc` scope allowlist and can never pass. The hook previously fed `origin/main`'s tip, which is diverged for any branch whose first push happens after `main` moved ahead of its branch point, so it rejected correct branches with failures the author did not write. `git merge-base` is an ancestor by construction; don't "simplify" it back to the tip.

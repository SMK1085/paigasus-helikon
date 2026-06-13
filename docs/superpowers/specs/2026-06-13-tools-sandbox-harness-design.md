# SMA-328 — `paigasus-helikon-tools`: sandboxed Read/Write/Edit/Bash harness

**Status:** design approved, pending spec review
**Ticket:** [SMA-328](https://linear.app/smaschek/issue/SMA-328/paigasus-helikon-tools-sandboxed-readwritebashwebfetch-harness)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-328-paigasus-helikon-tools-sandboxed-readwritebashwebfetch`
**Date:** 2026-06-13

## 1. Summary

Implement `paigasus-helikon-tools`, the companion crate that ports the Claude
Agent SDK harness pattern to Rust: sandboxed filesystem and process primitives
that an `LlmAgent` can be equipped with. This ticket delivers the **filesystem +
process** subset — `ReadTool`, `WriteTool`, `EditTool`, `BashTool` — plus the
shared `Sandbox` primitive they share. The network tools (`WebFetchTool`,
`WebSearchTool`) are deferred to a follow-up ticket (see §11); they bring a
distinct dependency footprint (reqwest, external search APIs, domain-allowlist
machinery) and are not needed to satisfy this ticket's acceptance criteria.

The crate ascends from its pre-published `0.0.0` name-claim stub to `0.1.0`.

## 2. Scope decisions (resolved during brainstorming)

These five decisions were made explicitly and drive the rest of the design.

1. **Two enforcement layers, not one.** The ticket's original wording ("all
   tools route through `PermissionPolicy::check`" and "denials surface as
   `ToolError::PermissionDenied`") predates the SMA-326 permission architecture
   and is stale. In the shipped system the **runner's control layer** already
   evaluates `deny rules → permission mode → PermissionPolicy → AskUser`
   *before* any tool's `invoke` runs (`core/src/control.rs:114`). Tools must
   **not** re-invoke `PermissionPolicy` — that would double-enforce. Tools are
   responsible only for their **own hard safety invariants** (path containment,
   read encoding, output caps, timeouts).

2. **Boundary violations surface via a new `ToolError::Denied { reason }`.** We
   add a structured `Denied` variant to `paigasus-helikon-core` (honoring the
   ticket's intent, renamed off the stale "Permission" framing, since these are
   *not* policy denials). Accepted cost: this is core API used by the ascending
   crate in the same PR, so the **5-step ascend** applies (§10).

3. **PR scope: filesystem + Bash now; web tools follow-up.** `Sandbox`,
   `ReadTool`, `WriteTool`, `EditTool`, `BashTool`, the core `Denied` variant,
   and the demo land here. `WebFetchTool`/`WebSearchTool` get their own ticket.

4. **Bash gets *soft* confinement, documented as not a hard jail.** cwd pinned
   to the sandbox root, configurable timeout (process-group kill on expiry),
   env scrubbed to an allowlist, optional command allow/deny, output-size cap.
   The real defense for Bash is the runner's `PermissionPolicy`/deny-rules +
   human approval — a spawned shell can read anything the process can, and that
   is stated plainly in the docs.

5. **Sandbox containment is OS-enforced via `cap-std`.** The sandbox root is
   opened as a `cap_std::fs::Dir`; all FS ops resolve openat-relative and the OS
   rejects absolute paths, `..` traversal, and symlink escapes — eliminating the
   symlink/TOCTOU bug class that hand-rolled prefix checks are prone to.

6. **AC demo: deterministic mock-model CI test + a real-model example.** A
   `ScriptedModel` integration test proves the wiring in CI with no API key; a
   manual `examples/` binary against `OpenAiModel` is the human-facing demo.

## 3. Integration surface (existing core APIs we build against)

Verified against the current tree:

- **`Tool<Ctx>` trait** (`core/src/tool.rs:64`) — `#[async_trait]`; methods
  `name()`, `description()`, `schema() -> &serde_json::Value`,
  `output_schema()` (default `None`), `effect() -> ToolEffect` (default
  `SideEffect`), and `async fn invoke(&self, ctx: &ToolContext<Ctx>, args:
  serde_json::Value) -> Result<ToolOutput, ToolError>`. Object-safe; agents hold
  `Arc<dyn Tool<Ctx>>`.
- **`ToolEffect`** (`core/src/tool.rs:19`) — `ReadOnly | Write | SideEffect`.
  Declaring it correctly unlocks `Plan` (read-only) and `AcceptEdits` (write
  auto-approve) modes.
- **`ToolOutput`** (`core/src/tool.rs:235`) — `{ content: serde_json::Value }`,
  constructed via `ToolOutput::new(content)`.
- **`ToolError`** (`core/src/tool.rs:253`) — currently `InvalidArgs {
  schema_errors }` (the only recoverable variant per ADR-10) and
  `Other(#[from] anyhow::Error)`. We add `Denied { reason }` (§7).
- **`AgentBuilder`** (`core/src/agent_builder.rs:119`) — `.tool(impl Tool<Ctx>)`,
  `.shared_tool(Arc<dyn Tool<Ctx>>)`, `.tools(IntoIterator)`.
- **`McpTool<Ctx>`** (`mcp/src/client/tool.rs:70`) — precedent for the
  phantom-`Ctx` pattern: a tool that ignores the user context carries
  `PhantomData<fn() -> Ctx>` so one value serves agents of any context type.
- **Conventions:** `#[async_trait]` on all async traits; `thiserror` for public
  errors + `anyhow` escape hatch; `#[non_exhaustive]` on public enums; tool
  input schemas are plain `serde_json::Value` (schemars-generated upstream).

## 4. Crate layout

```
crates/paigasus-helikon-tools/
  Cargo.toml          # ascend 0.0.0 → 0.1.0; deps §9; [lints] workspace = true
  src/
    lib.rs            # crate docs + re-exports
    sandbox.rs        # Sandbox primitive + SandboxError
    read.rs           # ReadTool
    write.rs          # WriteTool
    edit.rs           # EditTool
    bash.rs           # BashTool + BashToolBuilder
  tests/
    sandbox.rs              # containment unit tests (escape attempts → Denied)
    read_write_edit.rs      # per-tool behavior
    bash.rs                 # timeout, env scrub, output cap, exit codes
    sandbox_navigation.rs   # AC: #[cfg(unix)] ScriptedModel over a temp sandbox
  examples/
    explore_sandbox.rs      # real OpenAiModel demo (manual, not CI)
```

Public re-exports from `lib.rs`: `Sandbox`, `SandboxError`, `ReadTool`,
`WriteTool`, `EditTool`, `BashTool`, `BashToolBuilder`. Every `pub` item carries
a `///` doc comment (workspace `missing_docs = "warn"` + `-D warnings` in CI).

## 5. The `Sandbox` primitive

A cheaply-cloneable handle shared by all FS tools and used as Bash's cwd.

```rust
#[derive(Clone)]
pub struct Sandbox {
    inner: Arc<SandboxInner>,
}

struct SandboxInner {
    root: PathBuf,            // canonical root, for diagnostics + Bash cwd
    dir: cap_std::fs::Dir,    // capability handle; all FS ops go through this
}

impl Sandbox {
    /// Open `root` as a capability-confined sandbox. Every filesystem
    /// operation performed by tools built on this sandbox is resolved
    /// relative to `root` and cannot escape it (absolute paths, `..`, and
    /// symlinks leaving the root are rejected by the OS).
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SandboxError>;

    /// The sandbox root on the host filesystem (diagnostics / Bash cwd).
    pub fn root(&self) -> &Path;

    pub(crate) fn dir(&self) -> &cap_std::fs::Dir;
}
```

`Sandbox::open` uses `Dir::open_ambient_dir(root, ambient_authority())` and
records the canonical `root`. `cap_std::fs::Dir` is `Send + Sync` (it wraps a
directory fd), so `Arc<SandboxInner>` is freely shareable across the async
tools.

`SandboxError` (crate-local `thiserror` enum, `#[non_exhaustive]`) covers
**construction** failures only — root does not exist, is not a directory,
permission denied. This is distinct from in-`invoke` denials, which use core's
`ToolError::Denied`.

## 6. The four tools

All tools are generic over `Ctx` via `PhantomData<fn() -> Ctx>` (the `McpTool`
pattern) and implement `Tool<Ctx>`. Each input type derives `serde::Deserialize +
schemars::JsonSchema`; the JSON schema is generated once at construction and
stored as a `serde_json::Value` so `schema()` returns a borrow. Arguments that
fail to deserialize map to `ToolError::InvalidArgs { schema_errors }`.

FS work runs inside `tokio::task::spawn_blocking` (cap-std is synchronous),
moving a cloned `Arc<SandboxInner>` into the closure.

### 6.1 `ReadTool`
- `name() = "Read"`, `effect() = ReadOnly`.
- Input: `{ path: String, offset: Option<u64>, limit: Option<u64> }`
  (`offset`/`limit` are 1-based line window semantics, mirroring Claude Code's
  `Read`; absent ⇒ whole file).
- Reads a UTF-8 text file relative to the sandbox root. Path escape ⇒
  `Denied`. Non-UTF-8 content ⇒ `Denied { reason: "file is not valid UTF-8" }`
  (deliberate refusal to return binary as text). Missing file / I/O error ⇒
  `ToolError::Other` (operational failure, surfaced to the model).
- Output: `{ "content": "<text>" }` (line-windowed when requested).

### 6.2 `WriteTool`
- `name() = "Write"`, `effect() = Write`.
- Input: `{ path: String, content: String }`.
- Creates or overwrites a file relative to root, creating intermediate
  directories **inside** the sandbox as needed. Escape ⇒ `Denied`.
- Output: `{ "path": "<rel>", "bytes_written": <n> }`.

### 6.3 `EditTool`
- `name() = "Edit"`, `effect() = Write`.
- Input: `{ path, old_string, new_string, replace_all: Option<bool> }`.
- Exact string replacement. `old_string` absent ⇒ `Denied`. `old_string`
  non-unique and `replace_all != true` ⇒ `Denied { reason: "old_string is not
  unique; pass replace_all or add context" }`. Mirrors Claude Code's `Edit`.
- Output: `{ "path": "<rel>", "replacements": <n> }`.

### 6.4 `BashTool`
- `name() = "Bash"`, `effect() = SideEffect`.
- Input: `{ command: String }`.
- Built via `BashToolBuilder`:
  ```rust
  BashTool::builder(sandbox)
      .timeout(Duration)          // default 30s
      .env_allowlist(["PATH", "HOME"])   // default; replaces inherited env
      .max_output_bytes(1 << 20)  // default 1 MiB; truncate + flag
      .allow_commands(..) / .deny_commands(..)  // optional prefix matchers
      .build()
  ```
- Spawns the platform shell with cwd = `sandbox.root()`:
  - unix: `sh -c <command>`, child placed in its own process group
    (`process_group(0)`); on timeout the **group** is killed.
  - Windows: `cmd /C <command>`; best-effort child kill on timeout
    (grandchildren may survive — documented).
- Uses `tokio::process::Command` + `tokio::time::timeout`. Captures
  stdout/stderr up to `max_output_bytes` (truncates and sets a flag past the
  cap).
- Output: `{ "stdout", "stderr", "exit_code": Option<i32>, "timed_out": bool,
  "truncated": bool }`.
- **Docs state explicitly**: this is soft confinement, not a security boundary.
  Pair with `PermissionPolicy` / `DenyRule::tool("Bash")` for real control.

## 7. Error model & the core change

In-`invoke` outcomes map as follows:

| Condition | `ToolError` variant |
|-----------|---------------------|
| Args fail schema/deserialize | `InvalidArgs { schema_errors }` (recoverable) |
| Path escapes root; non-UTF-8 read; Edit `old_string` absent or non-unique without `replace_all`; Bash command blocked by an allow/deny rule | `Denied { reason }` (**new**) |
| Missing file; unexpected I/O; shell spawn failure | `Other(anyhow::Error)` |

`Denied` is for a *deliberate refusal* — a safety-boundary violation or an
unsatisfiable precondition the tool will not proceed past. Operational failures
(file not found, OS I/O errors) use `Other`.

**Bash soft outcomes are reported in the `ToolOutput`, not as errors:** a
non-zero exit code (`exit_code`), a killed-on-timeout run (`timed_out: true`),
and output truncation past the cap (`truncated: true`) are all normal results
the model inspects — only an allow/deny-blocked command is a `Denied`.

New variant added to `core/src/tool.rs`'s `ToolError`:

```rust
/// The tool refused the operation: either a hard safety-boundary violation
/// (a path outside the sandbox root, a non-UTF-8 read) or an unsatisfiable
/// precondition (an ambiguous edit target, an allow/deny-blocked command).
/// Distinct from a `PermissionPolicy` denial, which the runner resolves
/// before `invoke` is ever called. Not recoverable.
#[error("operation denied: {reason}")]
Denied {
    /// Human-readable denial reason, surfaced to the model.
    reason: String,
},
```

Additive on a `#[non_exhaustive]` enum ⇒ semver-compatible. **Implementation
must verify** the runner's `ToolError` handling surfaces `Denied` to the model
the same way it surfaces `Other` (reported as a tool result, non-recoverable) —
and specifically does *not* treat it like the recoverable `InvalidArgs`. If the
runner matches `ToolError` exhaustively-with-wildcard, no change is needed beyond
confirming the wildcard arm's behavior is the intended one.

## 8. Testing & the demo

- **`tests/sandbox_navigation.rs` (CI, the AC), `#[cfg(unix)]`:** an in-crate
  `ScriptedModel` implementing core's `Model` trait emits a fixed tool-call
  sequence (e.g. `Bash("ls")` → `Read(<file>)` → final answer) driving a real
  `LlmAgent` over a `tempfile::tempdir()` sandbox; asserts the final answer
  reflects the directory contents and that denied operations produce a `Denied`
  tool result. Deterministic, no API key.
- **`tests/sandbox.rs`:** adversarial containment — `Read("../../etc/passwd")`,
  an absolute path, and a symlink pointing outside the root all assert
  `ToolError::Denied`.
- **`tests/read_write_edit.rs`, `tests/bash.rs`:** per-tool behavior — line
  windows, overwrite, parent-dir creation, unique/`replace_all` edit, Bash
  timeout, env scrubbing, output truncation, exit codes. Bash tests use
  portable commands or split by `cfg`.
- **`examples/explore_sandbox.rs` (manual, not CI):** `OpenAiModel::chat(
  "gpt-5-mini").build()?` + the four tools over a real directory, behind
  `OPENAI_API_KEY`. `paigasus-helikon-providers-openai` is a **path-only
  dev-dependency** (consistent with the SMA-326 internal-dev-dep convention).
  Examples/tests are not built during `cargo publish --verify`, so this does not
  affect the release (to be confirmed during implementation).

## 9. Dependencies

Add to `[workspace.dependencies]` (root) and reference via `dep.workspace =
true`:

- `cap-std` — capability-based sandbox. Verify MSRV ≤ 1.85.
- `async-trait`, `serde` (derive), `serde_json`, `schemars` — already used
  elsewhere; reuse the existing workspace pins.
- `tokio` — with `process`, `time`, `rt` features (for `spawn_blocking`,
  `Command`, `timeout`).
- `thiserror` — for `SandboxError`.
- `anyhow` — for `ToolError::Other` escape hatches.

Dev-dependencies (crate): `tempfile`, `tokio` (test macros / `rt`), and
`paigasus-helikon-providers-openai` (path-only) for the example.

**`deny.toml`:** add `Apache-2.0 WITH LLVM-exception` to the license allowlist
(cap-std), and confirm its transitive licenses pass the `deny` gate. Commit the
resulting `Cargo.lock` update.

## 10. Release mechanics — the 5-step ascend (+ facade)

Because we add core API (`ToolError::Denied`) consumed by the ascending crate in
the same PR, the 5-step ascend applies, and the facade is bumped too (the
second-order drift caveat in CLAUDE.md):

1. **core:** add `ToolError::Denied`; patch-bump `paigasus-helikon-core`; update
   its CHANGELOG and the `[workspace.dependencies]` core pin. (release-plz
   publishes core first, so `-tools` verifies against the fresh core.)
2. **tools:** `version = "0.0.0" → "0.1.0"`; remove `publish = false` from
   `crates/paigasus-helikon-tools/Cargo.toml`.
3. **release-plz.toml:** remove the `-tools` `[[package]] … release = false`
   block.
4. **facade:** add the optional `paigasus-helikon-tools` dep + a `tools` feature
   + a `pub use` re-export (kebab feature `tools` ↔ snake re-export; doc the
   re-export); patch-bump `paigasus-helikon` + its self-pin + CHANGELOG.
5. Land the version/gate changes as one
   `chore(release): SMA-328 lift stage-1 gates for tools` commit alongside the
   implementation on the feature branch.

## 11. Follow-up ticket (web tools)

After this spec is approved, file a Linear ticket under the **Composition &
Extensibility** milestone for `WebFetchTool` + `WebSearchTool`: reqwest-based
HTTP fetch with domain allow/deny lists and a max-body cap, plus a pluggable
search-backend trait (Brave/Tavily) with at least one real backend. It will
reference this spec for the shared `Tool`/error conventions.

## 12. Out of scope (YAGNI)

- Hard OS-level Bash jailing (landlock / seccomp / `sandbox-exec` / job objects)
  and CPU/memory rlimits.
- `WebFetchTool` / `WebSearchTool` (deferred — §11).
- A virtual/in-memory filesystem backend.
- Concurrent-edit conflict detection beyond the unique-match check.

## 13. Acceptance criteria (restated against this design)

- A demo agent equipped with `ReadTool`, `WriteTool`, `EditTool`, `BashTool` can
  navigate a sandbox directory and answer questions about its contents
  (`tests/sandbox_navigation.rs`, plus the runnable example).
- Sandbox-boundary violations surface as `ToolError::Denied { reason }` (the
  updated, accurate restatement of the ticket's original
  `PermissionDenied` criterion).
- All CI gates green (fmt, clippy, test matrix incl. the `cfg`-split Bash tests,
  docs, doc-coverage, commits, pr-title, audit, deny).
```


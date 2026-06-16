# Tools

A *tool* is a function the model can call mid-run. Tools come in two layers:

1. **Your own tools** — defined against the `Tool` trait, usually via the `#[tool]`
   attribute macro, and registered with the `tools!` macro.
2. **Ready-made sandboxed tools** — filesystem and shell tools shipped in
   `paigasus-helikon-tools` (feature `tools`), plus network tools behind `tools-web`.

For a runnable end-to-end agent, see the [quickstart](../getting-started/quickstart.md).

## The `Tool` trait

`Tool<Ctx>` (in `paigasus_helikon::core`) is object-safe so applications can hold a
heterogeneous registry as `Vec<Arc<dyn Tool<Ctx>>>`. A tool reports its name,
description, and argument schema to the model, and runs in `invoke`:

```rust,ignore
#[async_trait]
pub trait Tool<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> &serde_json::Value;
    fn output_schema(&self) -> Option<&serde_json::Value> { None }
    fn effect(&self) -> ToolEffect { ToolEffect::SideEffect }

    async fn invoke(
        &self,
        ctx: &ToolContext<Ctx>,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}
```

`effect` returns a `ToolEffect` (`ReadOnly`, `Write`, or the default `SideEffect`).
It drives `PermissionMode` decisions: `Plan` allows only `ReadOnly`, and
`AcceptEdits` auto-approves `Write`. An undeclared tool is treated as
side-effecting, so `Plan` mode blocks it. A successful call returns a `ToolOutput`
whose `content` is the raw JSON the tool produced.

You can implement `Tool` by hand, but for an `async fn` the `#[tool]` macro is the
ergonomic path.

## Defining a tool with `#[tool]`

`#[tool]` (re-exported as `paigasus_helikon::tool` under the `macros` feature) turns
an `async fn` into a value implementing `Tool<Ctx>`. The argument struct derives
`serde::Deserialize` + `schemars::JsonSchema`; the return struct derives
`serde::Serialize` + `schemars::JsonSchema`. The function's `///` doc comment
becomes the tool description shown to the model, and the function name becomes the
tool name.

```rust,ignore
use paigasus_helikon::core::{ToolContext, ToolError};
use paigasus_helikon::{tool, tools};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct LookupSpendingArgs {
    /// Spending category, e.g. "Dining".
    category: String,
    /// Month in YYYY-MM form.
    month: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct LookupSpendingOut {
    /// Total spent in the category this month, in dollars.
    total: f64,
    /// Number of transactions.
    count: u32,
}

/// Look up the user's total spending and transaction count for a category in a month.
#[tool]
async fn lookup_spending(
    _ctx: &ToolContext<()>,
    args: LookupSpendingArgs,
) -> Result<LookupSpendingOut, ToolError> {
    let out = match args.category.to_lowercase().as_str() {
        "dining" => LookupSpendingOut { total: 312.40, count: 18 },
        "groceries" => LookupSpendingOut { total: 540.10, count: 9 },
        _ => LookupSpendingOut { total: 0.0, count: 0 },
    };
    Ok(out)
}
```

The full example lives at
`crates/paigasus-helikon/examples/budget_assistant_openai.rs`.

### `ToolContext<Ctx>`

The first parameter is `&ToolContext<Ctx>` — a narrower view of the run's
`RunContext`. `Ctx` is your application context type (`()` when you need none).
`ToolContext` deliberately excludes the session handle so tools cannot bypass the
runner's persistence. It exposes:

- `user_ctx() -> &Arc<Ctx>` — your application context.
- `state() -> &SessionState` — run-scoped state shared across sub-agents.
- `actions() -> &ActionsHandle` — e.g. `ctx.actions().escalate()` to stop an
  enclosing `LoopAgent`.
- `permission_mode() -> PermissionMode` — a tool may branch on this.
- `tracer()`, `cancel()`, `agent_depth()`, `max_agent_depth()`.

### `ToolError`

`invoke` returns `Result<_, ToolError>`. The variants:

- `InvalidArgs { schema_errors }` — arguments did not match the schema. This is the
  one recoverable variant: the runner may feed the errors back to the model once.
- `Denied { reason }` — the tool refused (a safety-boundary violation, e.g. a path
  outside the sandbox, or an unsatisfiable precondition). Not recoverable.
- `Other(anyhow::Error)` — escape hatch for arbitrary failures (`#[from]`, so `?` on
  an `anyhow::Error` works).

## Registering tools with `tools!`

`tools!` (re-exported as `paigasus_helikon::tools` under the `macros` feature) boxes
a comma-separated list of tool values into `Vec<Arc<dyn Tool<Ctx>>>`. Pass the bare
tool values — do **not** pre-wrap with `Arc`. Every tool in one invocation must
implement `Tool<Ctx>` for the *same* `Ctx`.

```rust
let agent = LlmAgent::builder::<()>()
    .name("budget-assistant")
    .model(model)
    .instructions("You are a budgeting assistant. Use the tools …")
    .tools(tools![lookup_spending, budget_status])
    .build();
```

The builder also has a singular `.tool(t)` for registering one tool at a time.

> The `tools` name is overloaded: with the `macros` feature `paigasus_helikon::tools`
> is the `tools!` macro; with the `tools` feature it is the sandboxed-tools crate
> module. They live in different namespaces, so Rust resolves each correctly.

## The ready-made sandboxed toolset (`tools` feature)

`paigasus-helikon-tools` (facade feature `tools`) ships filesystem and shell tools
that an agent can use to inspect and modify a project. The four exported tool types
report these names to the model: `ReadTool` (`"Read"`), `WriteTool` (`"Write"`),
`EditTool` (`"Edit"`), and `BashTool` (`"Bash"`).

```rust,ignore
use paigasus_helikon_tools::{BashTool, EditTool, HostBackend, ReadTool, Sandbox, WriteTool};

let sandbox = Sandbox::open(".")?;

let agent = LlmAgent::builder::<()>()
    .name("sandbox-explorer")
    .model(model)
    .instructions("You can inspect the sandbox with Read/Write/Edit/Bash. …")
    .tool(ReadTool::<()>::new(sandbox.clone()))
    .tool(WriteTool::<()>::new(sandbox.clone()))
    .tool(EditTool::<()>::new(sandbox.clone()))
    .tool(BashTool::<()>::new(HostBackend::builder(sandbox).build()))
    .build();
```

`ReadTool`, `WriteTool`, and `EditTool` take a `Sandbox` via `::new(sandbox)`.
`BashTool` takes an `Arc<dyn ExecutionBackend>` — use `HostBackend::builder(sandbox).build()`
for the default unconfined backend, or `OsSandboxBackend::builder(sandbox).build()`
(Linux, feature `os-sandbox`) for OS-enforced containment. `BashToolBuilder` exposes
`allow_commands` and `deny_commands` for command-level filtering. The full example is
`crates/paigasus-helikon-tools/examples/explore_sandbox.rs`.

### Confinement model

A `Sandbox` is a directory opened as an OS-confined capability via `cap-std`
(`Sandbox::open(root)`). `ReadTool` (`ReadOnly`), `WriteTool` (`Write`), and
`EditTool` (`Write`) operate strictly inside it — they cannot escape via `..`,
absolute paths, or symlinks; an attempt yields `ToolError::Denied`.

`BashTool` is the exception: it is a **cwd-pinned shell, not a security sandbox**.
The `cap-std` containment does not extend to a spawned child process, so a command
can read and write anything this process can — absolute paths, `..`, `~`, and the
network. Its effect is `SideEffect`, and in `PermissionMode::Default` with no
`PermissionPolicy` installed it runs ungated. Gate it with a `PermissionPolicy` or a
`DenyRule::tool("Bash")` for real control — `explore_sandbox.rs` demonstrates the
former.

## Containment vs approval

`BashTool` separates three independent axes that are often conflated:

- **Containment** — what OS-kernel mechanisms prevent the spawned process from
  accessing resources it was not granted. Enforced by the `ExecutionBackend`.
- **Approval** — whether a human or a `PermissionPolicy` must authorise the call
  before it runs. Enforced by the runner's permission pipeline.
- **Resource-capping** — CPU time, file-size, and address-space limits applied via
  `setrlimit` so a runaway command does not exhaust the host.

These are orthogonal: you can grant full filesystem access (no containment) while
requiring human approval (strict approval), or jail a command to a tmpdir
(containment) and run it without asking (no approval required). Choose each axis
independently.

### Execution backends

`BashTool` delegates execution to a value implementing `ExecutionBackend`. Swap the
backend to change the containment tier without touching any other part of your agent.

#### `OsSandboxBackend` (Linux; feature `os-sandbox`) — recommended for untrusted commands

The strongest containment tier. Built in the parent process; applied in the child via
a `pre_exec` hook. **Fail-closed**: `build()` returns `Err(OsSandboxError::Unsupported(…))`
if the kernel cannot enforce the requested isolation, so a misconfigured host is
never silently left unprotected.

```rust,ignore
use paigasus_helikon_tools::{BashTool, OsSandboxBackend, Sandbox};

let sandbox = Sandbox::open("./workspace")?;
let backend = match OsSandboxBackend::builder(sandbox).build() {
    Ok(b) => b,
    Err(e) => {
        // Fall back to HostBackend when Landlock is unavailable (e.g. a macOS
        // dev machine or a kernel older than 5.13).
        eprintln!("OS sandbox unavailable ({e}); falling back to host backend");
        HostBackend::builder(Sandbox::open("./workspace")?).build()
    }
};
let tool = BashTool::<()>::new(backend);
```

What `OsSandboxBackend` enforces:

| Axis | Mechanism | Guarantee |
|---|---|---|
| Filesystem | Landlock (LSM, kernel ≥ 5.13) | Read+write only under the sandbox root; read-only for a system path set (`/usr`, `/bin`, `/lib`, …). Attempts to write outside the root fail at the OS layer — not just at the shell level. |
| Network | seccomp-bpf | `socket(AF_INET)` and `socket(AF_INET6)` return `EPERM` by default. `AF_UNIX` (local sockets) is allowed. Pass `.allow_network(true)` to lift the IP egress restriction. |
| Syscalls | seccomp-bpf | A small deny-list of dangerous syscalls (`ptrace`, `mount`, `pivot_root`, `chroot`, `setns`, `unshare`, `kexec_load`, `bpf`, `perf_event_open`) always returns `EPERM`. |
| Resource | `setrlimit` | Configured via `.rlimits(ResourceLimits { … })`; defaults apply a CPU backstop and a 1 GiB file-size cap. |

**Kernel requirements:** Linux ≥ 5.13 (Landlock ABI v1); x86_64 or aarch64 (seccomp
BPF is architecture-specific). No Linux namespaces or privileged capabilities are
needed — the entire mechanism is unprivileged.

`OsSandboxBackend::guarantees()` returns:

```rust,ignore
SandboxGuarantees {
    filesystem: Isolation::OsKernel,
    network:    Isolation::OsKernel,  // or Isolation::None if .allow_network(true)
    syscalls:   Isolation::OsKernel,
    label:      "os-sandbox (landlock+seccomp)",
}
```

**Forthcoming:** domain-level network egress proxy (route all outbound traffic
through a policy-enforcing proxy rather than blocking at the socket layer) and macOS
Seatbelt containment are tracked in SMA-426.

#### `HostBackend` (all platforms) — default, unconfined

The default backend. Pins the working directory to the sandbox root and scrubs the
environment to a configurable allowlist, but spawned commands have the same OS
access as the parent process.

```rust,ignore
use paigasus_helikon_tools::{BashTool, HostBackend, Sandbox};

let backend = HostBackend::builder(Sandbox::open("./workspace")?)
    .timeout(std::time::Duration::from_secs(10))
    .env_allowlist(["PATH", "HOME"])
    .build();
let tool = BashTool::<()>::new(backend);
```

`HostBackend::guarantees()` returns:

```rust,ignore
SandboxGuarantees {
    filesystem: Isolation::None,
    network:    Isolation::None,
    syscalls:   Isolation::None,
    label:      "host (no containment)",
}
```

> **`HostBackend` is NOT a security boundary.** A command it runs can read and
> write anything the parent process can. Pair it with a `PermissionPolicy` or a
> `DenyRule::tool("Bash")` for approval-level control, or use `OsSandboxBackend`
> for OS-enforced containment.

### `guarantees()` tiers

`ExecutionBackend::guarantees()` returns a `SandboxGuarantees` struct with an
`Isolation` value on each axis:

- `Isolation::None` — no OS enforcement; the command has the same access as the
  parent process on that axis.
- `Isolation::OsKernel` — enforced by an OS kernel mechanism (Landlock LSM for
  filesystem; seccomp-bpf for network and syscalls). A violating operation returns
  an OS error — the command cannot bypass it from userspace.

The `label` field is a short human-readable string (`"host (no containment)"` /
`"os-sandbox (landlock+seccomp)"`) that `BashTool` surfaces in its tool description
so the model knows what tier it is operating under.

### Network tools (`tools-web` feature)

The facade feature `tools-web` (the tools crate's own `web` feature) adds two
network tools, re-exported from `paigasus_helikon_tools`:

- `WebFetchTool` (name `"WebFetch"`) — fetches an HTTP(S) URL, extracts the main
  article, and returns Markdown. Built via `WebFetchTool::builder()`.
- `WebSearchTool` (name `"WebSearch"`) — runs a query through a swappable
  `SearchBackend`. Built via `WebSearchTool::builder(backend)`; the crate provides
  `BraveBackend` and `TavilyBackend` implementations, with each hit modeled as a
  `SearchResult`.

`WebFetchTool` enforces an optional host allow/deny list **and** a default-on SSRF
guard: it blocks private, loopback, link-local (including the cloud-metadata IP),
CGNAT, and IPv6 ULA addresses, and it re-validates resolved IPs at connect time to
close the DNS-rebinding window. Both web tools report `SideEffect`.

## See also

- [Quickstart](../getting-started/quickstart.md) — a complete first agent.
- [`paigasus-helikon-tools` on docs.rs](https://docs.rs/paigasus-helikon-tools) and
  [`paigasus-helikon-macros`](https://docs.rs/paigasus-helikon-macros).

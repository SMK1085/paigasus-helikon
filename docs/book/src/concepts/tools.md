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
for the default unconfined backend, `OsSandboxBackend::builder(sandbox).build()`
(Linux + macOS, feature `os-sandbox`) for OS-enforced containment, or
`ForkdBackend::builder(controller_url).bearer_token(token).snapshot(tag).build()?`
(Linux KVM, feature `microvm`, experimental) for microVM-level isolation — unlike the
other backends it takes a forkd controller URL, not a `Sandbox`; add
`.egress_policy(…).enforce_egress(proxy_endpoint)` to reach `Isolation::Proxied` on the
network axis via `EgressProxy` (see the runbook). `BashToolBuilder` exposes
`allow_commands` and `deny_commands` for command-level filtering. The full example is
`crates/paigasus-helikon-tools/examples/explore_sandbox.rs`.

### Confinement model

A `Sandbox` is a directory opened as an OS-confined capability via `cap-std`
(`Sandbox::open(root)`). `ReadTool` (`ReadOnly`), `WriteTool` (`Write`), and
`EditTool` (`Write`) operate strictly inside it — they cannot escape via `..`,
absolute paths, or symlinks; an attempt yields `ToolError::Denied`.

`BashTool` is different: its containment depends on the `ExecutionBackend` it is
given (see [Containment vs approval](#containment-vs-approval) below). With the
default `HostBackend` it is a **cwd-pinned shell, not a security sandbox** — the
`cap-std` containment does not extend to a spawned child process, so a command can
read and write anything this process can (absolute paths, `..`, `~`, and the
network). Its effect is `SideEffect`, and in `PermissionMode::Default` with no
`PermissionPolicy` installed it runs ungated: gate it with a `PermissionPolicy` or a
`DenyRule::tool("Bash")` (as `explore_sandbox.rs` demonstrates), or use
`OsSandboxBackend` for OS-enforced containment, or `ForkdBackend` (feature
`microvm`, experimental) for microVM-level isolation with optional `Isolation::Proxied`
network enforcement via `EgressProxy`.

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

#### `ForkdBackend` (Linux KVM; feature `microvm`) — microVM containment, experimental

The strongest containment tier on the filesystem and syscall axes: each command runs
inside a KVM-isolated Firecracker microVM orchestrated by the forkd daemon. The
`ForkdBackend` itself is a portable REST client (no Linux-kernel dependency in the
client crate); the daemon side requires Linux + `/dev/kvm`. Platform availability is
checked at runtime when controller requests are made (for example on `run()`), not
at compile time.

> **Experimental.** The fork → exec → destroy REST flow is implemented and
> mock-tested (SMA-416). Network egress enforcement via `EgressProxy` is now
> implemented (SMA-437) but requires a live deployment — see
> `docs/runbooks/forkd-live-validation.md` in the repository.
> **Do not enable `microvm` in production without completing the deployment
> checklist in the runbook.**

**Network containment — layered model (SMA-437).** The `microvm` network guarantee
now has two states depending on whether the layered egress enforcement is deployed:

- **`Isolation::None` (default)** — no network enforcement is in place. The microVM
  can reach any host the host network allows. This is the state when
  `ForkdBackend::builder(…).build()` is called *without* `.enforce_egress(…)`.
- **`Isolation::Proxied` (enforced)** — all HTTP/S egress is domain-filtered at the
  [`EgressProxy`](https://docs.rs/paigasus-helikon-tools/latest/paigasus_helikon_tools/struct.EgressProxy.html)
  (application layer). Meaningful **only in the layered deployment**: a per-VM netns
  default-deny (iptables) that routes all egress through the proxy, so non-proxy-aware
  clients, UDP/53 DNS, QUIC/HTTP-3, and raw TCP cannot escape. The backend itself
  cannot verify the host's netns rules; this tier reflects an **operator attestation**
  via `.enforce_egress(proxy_endpoint)` (the same trust model the kernel/hypervisor
  tiers use for their respective boundaries). A reachability probe to the proxy is run
  at build time and fails closed if the proxy is unreachable.

To reach `Isolation::Proxied`:

```rust,ignore
// Requires the layered deployment described in the runbook.
let backend = ForkdBackend::builder("https://controller:8889")
    .bearer_token(token)
    .snapshot("helikon")
    .egress_policy(EgressPolicy::deny_all().allow_domains(["example.com"]))
    .enforce_egress("proxy-host:8443")   // attest + probe
    .build()?;

assert_eq!(backend.guarantees().network, Isolation::Proxied);
```

`ForkdBackend::guarantees()` — un-enforced (default):

```rust,ignore
SandboxGuarantees {
    filesystem: Isolation::Virtualized,
    network:    Isolation::None,       // default — deploy EgressProxy to reach Proxied
    syscalls:   Isolation::Virtualized,
    label:      "forkd (firecracker microvm — experimental)",
}
```

`ForkdBackend::guarantees()` — with `.enforce_egress()`:

```rust,ignore
SandboxGuarantees {
    filesystem: Isolation::Virtualized,
    network:    Isolation::Proxied,    // layered netns default-deny + EgressProxy deployed
    syscalls:   Isolation::Virtualized,
    label:      "forkd (firecracker microvm — experimental)",
}
```

#### `OsSandboxBackend` (Linux + macOS; feature `os-sandbox`) — recommended for untrusted commands

The strongest containment tier available on the current platform. Built in the parent
process; applied in the child via a `pre_exec` hook. **Fail-closed**: `build()`
returns `Err(OsSandboxError::Unsupported(…))` if the platform cannot enforce the
requested isolation, so a misconfigured host is never silently left unprotected.

```rust,ignore
use paigasus_helikon_tools::{BashTool, OsSandboxBackend, Sandbox};

let sandbox = Sandbox::open("./workspace")?;
// Fail-closed: `build()` errors if the OS cannot enforce the requested isolation,
// so containment is never silently downgraded. Propagate the error rather than
// dropping to an unconfined backend. A caller that genuinely wants a degraded mode
// can match on the error and opt in to `HostBackend` explicitly — but that is a
// deliberate, security-relevant choice, not a default.
let backend = OsSandboxBackend::builder(sandbox).build()?;
let tool = BashTool::<()>::new(backend);
```

What `OsSandboxBackend` enforces varies by platform:

**Linux** (kernel ≥ 5.13; x86_64 or aarch64):

| Axis | Mechanism | Guarantee |
|---|---|---|
| Filesystem | Landlock (LSM, kernel ≥ 5.13) | Read+write only under the sandbox root; read-only for a system path set (`/usr`, `/bin`, `/lib`, …). Attempts to write outside the root fail at the OS layer — not just at the shell level. |
| Network | seccomp-bpf | `socket(AF_INET)` and `socket(AF_INET6)` return `EPERM` by default. `AF_UNIX` (local sockets) is allowed. Pass `.allow_network(true)` to lift the IP egress restriction. |
| Syscalls | seccomp-bpf | A small deny-list of dangerous syscalls (`ptrace`, `mount`, `pivot_root`, `chroot`, `setns`, `unshare`, `kexec_load`, `bpf`, `perf_event_open`) always returns `EPERM`. |
| Resource | `setrlimit` | Configured via `.rlimits(ResourceLimits { … })`; defaults apply a CPU backstop and a 1 GiB file-size cap. |

No Linux namespaces or privileged capabilities are needed — the entire mechanism is
unprivileged.

`OsSandboxBackend::guarantees()` on Linux returns:

```rust,ignore
SandboxGuarantees {
    filesystem: Isolation::OsKernel,
    network:    Isolation::OsKernel,  // or Isolation::None if .allow_network(true)
    syscalls:   Isolation::OsKernel,
    label:      "os-sandbox (landlock+seccomp)",
}
```

**macOS** (any version that ships `/usr/bin/sandbox-exec`):

The macOS backend uses **Seatbelt** (`sandbox-exec`), Apple's sandbox MAC framework.
`sandbox-exec` is Apple-deprecated but ships on every macOS release. The posture is
**write-focused**: filesystem _write_ operations are denied outside the sandbox root,
while reads are unrestricted (weaker than Linux's read+write containment). Network is
all-or-nothing: denied by default, which also blocks `AF_UNIX` local sockets; pass
`.allow_network(true)` to permit all socket families. Seatbelt is an operation MAC,
not a syscall filter, so `syscalls` is `None`.

| Axis | Mechanism | Guarantee |
|---|---|---|
| Filesystem | Seatbelt (sandbox-exec) | **Write-only containment**: writes outside the sandbox root are denied at the OS layer; reads are unrestricted. |
| Network | Seatbelt (sandbox-exec) | All sockets denied by default (including `AF_UNIX`). Pass `.allow_network(true)` to allow all outbound traffic. |
| Syscalls | — | No syscall filter; `Isolation::None`. |
| Resource | `setrlimit` | Same as Linux — configured via `.rlimits(ResourceLimits { … })`. |

`OsSandboxBackend::guarantees()` on macOS returns:

```rust,ignore
SandboxGuarantees {
    filesystem: Isolation::OsKernel,   // write-only; reads unrestricted
    network:    Isolation::OsKernel,   // or Isolation::None if .allow_network(true)
    syscalls:   Isolation::None,
    label:      "os-sandbox (seatbelt)",
}
```

> **macOS containment is weaker than Linux.** The `OsKernel` label on the filesystem
> axis means OS-enforced, but only for _writes_. A sandboxed command can still read
> arbitrary files. Use the Linux backend (or a dedicated Linux CI environment) when
> read isolation is required.

**Domain-level network egress filtering** (route outbound traffic through a
policy-enforcing `EgressProxy` rather than blocking at the socket layer) is available
for the `microvm` tier (SMA-437). See `ForkdBackend` and `Isolation::Proxied` above.

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
- `Isolation::OsKernel` — enforced by an OS kernel mechanism. The exact mechanism
  and strength depend on the platform: on Linux, Landlock LSM for filesystem and
  seccomp-bpf for network and syscalls (read+write containment); on macOS, Seatbelt
  for filesystem (write-only; reads unrestricted) and network. A violating operation
  returns an OS error — the command cannot bypass it from userspace.
- `Isolation::Virtualized` — enforced by a VM boundary (Firecracker microVM via
  `ForkdBackend`). The command runs inside a KVM guest; host filesystem and syscalls
  are inaccessible by construction. Network is separately gated — see
  `Isolation::Proxied` below.
- `Isolation::Proxied` — egress filtered at a CONNECT/HTTP proxy (`EgressProxy`)
  enforcing a domain allow/deny policy. Meaningful only in the **layered deployment**:
  a per-VM netns default-deny that forces all guest TCP through the proxy (UDP/QUIC
  cannot reach the proxy and are dropped at L3/L4). Without the netns rules, raw TCP
  and UDP escape. The backend cannot verify the host's netns configuration, so
  `Proxied` is an operator attestation — the same trust model `Virtualized` uses for
  the hypervisor boundary. See `ForkdBackendBuilder::enforce_egress`.

The `label` field is a short human-readable string (`"host (no containment)"` /
`"os-sandbox (landlock+seccomp)"` on Linux / `"os-sandbox (seatbelt)"` on macOS /
`"microvm (forkd/firecracker) [skeleton]"`) that `BashTool` surfaces in its tool
description so the model knows what tier it is operating under.

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

# paigasus-helikon-tools

Sandboxed filesystem and process tools for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. Provides `ReadTool`, `WriteTool`, `EditTool`, and `BashTool`, plus `WebFetchTool` / `WebSearchTool` behind the `web` feature, OS-enforced Bash containment behind the `os-sandbox` feature (Linux: Landlock + seccomp; macOS: Seatbelt), and microVM Bash containment via the forkd Firecracker controller behind the `microvm` feature (portable REST client; experimental — SMA-416/SMA-437).

The filesystem tools operate inside a `Sandbox` — a directory opened as an OS-confined capability (`cap-std`), so they cannot escape it via `..`, absolute paths, or symlinks.

`BashTool` delegates execution to a pluggable `ExecutionBackend`. Use `HostBackend` (default, all platforms) for a cwd-pinned shell with env scrubbing and resource limits, `OsSandboxBackend` (feature `os-sandbox`) for OS-kernel-enforced containment — Linux via Landlock (filesystem) + seccomp-bpf (syscalls and network) with read+write restriction; macOS via Seatbelt (`sandbox-exec`) with **write-only** containment (reads unrestricted) and an all-or-nothing network toggle — or `ForkdBackend` (feature `microvm`, experimental) for microVM-level containment via the forkd Firecracker controller with optional domain-filtered egress enforcement via `EgressProxy` (SMA-437).

### microVM egress enforcement (`microvm` feature, SMA-437)

`ForkdBackend` can report `Isolation::Proxied` on the network axis when the layered
egress enforcement is deployed: a per-VM netns default-deny (iptables) that routes
all guest HTTP/S through `EgressProxy`, which enforces an `EgressPolicy` (domain
allow/deny list + SSRF block on resolved IPs).

```rust
use paigasus_helikon_tools::{EgressPolicy, EgressProxy, ForkdBackend, Isolation};
use tokio::net::TcpListener;

// Run the proxy (standalone, or as part of the Docker harness).
let listener = TcpListener::bind("0.0.0.0:8443").await?;
let policy = EgressPolicy::deny_all().allow_domains(["example.com"]);
tokio::spawn(EgressProxy::new(policy.clone()).serve(listener));

// Build the backend — attest the proxy is deployed + probe it for reachability.
let backend = ForkdBackend::builder("https://controller:8889")
    .bearer_token(token)
    .snapshot("helikon")
    .egress_policy(policy)
    .enforce_egress("0.0.0.0:8443")   // fails closed if unreachable
    .build()?;

assert_eq!(backend.guarantees().network, Isolation::Proxied);
```

The `microvm` feature enables `reqwest`, `url`, and `tokio/net` + `tokio/io-util`
(needed for the async tunnel copy). For the full deployment checklist (Docker harness,
GCP launch, guest-image build, live validation tests) see
`docs/runbooks/forkd-live-validation.md` in the repository.

> **`HostBackend` is NOT a security boundary.** A command it runs can read and write anything this process can. Pair it with a `PermissionPolicy` (or a `DenyRule::tool("Bash")`) for approval-level control, or use `OsSandboxBackend` for OS-enforced containment.

## Install

```bash
cargo add paigasus-helikon-tools
# with the web tools (WebFetch / WebSearch):
cargo add paigasus-helikon-tools --features web
# with OS-enforced Bash containment (Linux: Landlock + seccomp; macOS: Seatbelt):
cargo add paigasus-helikon-tools --features os-sandbox
# with microVM Bash containment + egress enforcement via forkd/Firecracker (experimental — SMA-437):
cargo add paigasus-helikon-tools --features microvm
```

Most users enable the `tools` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead (and `tools-web` for the web tools), which re-exports this crate as `paigasus_helikon::tools`.

## Example

```rust
use paigasus_helikon_core::LlmAgent;
use paigasus_helikon_tools::{BashTool, EditTool, HostBackend, ReadTool, Sandbox, WriteTool};

// A directory opened as an OS-confined capability.
let sandbox = Sandbox::open("./workspace")?;

// `model` is any `Model` impl (e.g. from a provider crate).
let agent = LlmAgent::builder::<()>()
    .name("file-agent")
    .model(model)
    .tool(ReadTool::<()>::new(sandbox.clone()))
    .tool(WriteTool::<()>::new(sandbox.clone()))
    .tool(EditTool::<()>::new(sandbox.clone()))
    .tool(BashTool::<()>::new(HostBackend::builder(sandbox).build()))
    .build();
```

Runnable examples live in [`examples/`](https://github.com/SMK1085/paigasus-helikon/tree/main/crates/paigasus-helikon-tools/examples): `explore_sandbox` (FS + Bash, gated by a `PermissionPolicy`), `web_research` (the `web` tools), and `os_sandbox_demo` (OS-sandbox containment demo, Linux + macOS, requires `--features os-sandbox`).

## Safety

`BashTool`'s `deny_commands` and `allow_commands` lists are **operator-aware**: a deny rule blocks a program that appears in any sub-command of a compound or pipelined command string (`&&`, `||`, `;`, `|`, `sudo`, `bash -c`, etc.), and an allow list requires every sub-command's program to be listed. This prevents simple bypasses such as `echo ok && rm -rf .`.

When `BashTool` runs inside the agent loop, tool output is automatically scrubbed of secret-shaped strings (env vars whose names end in `_API_KEY`, `_TOKEN`, `_SECRET`, etc., plus any values registered with `RunContext::with_extra_secrets`) before the output re-enters the model context. See [Permissions, Guardrails & Hooks](https://smk1085.github.io/paigasus-helikon/concepts/permissions-guardrails-hooks.html) for the full rules and opt-out knobs.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-tools)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [tools](https://smk1085.github.io/paigasus-helikon/concepts/tools.html) and [permissions, guardrails & hooks](https://smk1085.github.io/paigasus-helikon/concepts/permissions-guardrails-hooks.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.

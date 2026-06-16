# paigasus-helikon-tools

Sandboxed filesystem and process tools for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. Provides `ReadTool`, `WriteTool`, `EditTool`, and `BashTool`, plus `WebFetchTool` / `WebSearchTool` behind the `web` feature.

The filesystem tools operate inside a `Sandbox` — a directory opened as an OS-confined capability (`cap-std`), so they cannot escape it via `..`, absolute paths, or symlinks.

> **`BashTool` is a cwd-pinned shell, not a security sandbox.** The `cap-std` containment that jails the filesystem tools does **not** extend to a spawned child process — a command can read and write anything this process can. Pair `BashTool` with a `PermissionPolicy` (or a `DenyRule::tool("Bash")`) for real control.

## Install

```bash
cargo add paigasus-helikon-tools
# with the web tools (WebFetch / WebSearch):
cargo add paigasus-helikon-tools --features web
```

Most users enable the `tools` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead (and `tools-web` for the web tools), which re-exports this crate as `paigasus_helikon::tools`.

## Example

```rust
use paigasus_helikon_core::LlmAgent;
use paigasus_helikon_tools::{BashTool, EditTool, ReadTool, Sandbox, WriteTool};

// A directory opened as an OS-confined capability.
let sandbox = Sandbox::open("./workspace")?;

// `model` is any `Model` impl (e.g. from a provider crate).
let agent = LlmAgent::builder::<()>()
    .name("file-agent")
    .model(model)
    .tool(ReadTool::<()>::new(sandbox.clone()))
    .tool(WriteTool::<()>::new(sandbox.clone()))
    .tool(EditTool::<()>::new(sandbox.clone()))
    .tool(BashTool::<()>::builder(sandbox).build())
    .build();
```

Runnable examples live in [`examples/`](https://github.com/SMK1085/paigasus-helikon/tree/main/crates/paigasus-helikon-tools/examples): `explore_sandbox` (FS + Bash, gated by a `PermissionPolicy`) and `web_research` (the `web` tools).

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-tools)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [tools](https://smk1085.github.io/paigasus-helikon/concepts/tools.html) and [permissions, guardrails & hooks](https://smk1085.github.io/paigasus-helikon/concepts/permissions-guardrails-hooks.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.

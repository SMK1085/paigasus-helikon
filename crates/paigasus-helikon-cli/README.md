# paigasus-helikon-cli

Command-line binaries (`helikon` and `paigasus-helikon`) for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents.

> **Internal — no stability guarantees.** This crate publishes as a binary crate (`cargo install` needs its lib target on crates.io too), but its lib API is not a supported surface — only the two binaries are.

## Install

```bash
cargo install paigasus-helikon-cli
```

installs both `helikon` and `paigasus-helikon` (an alias binary — same command tree).

## Subcommands

```text
helikon repl      [--agents agents.toml] [--agent NAME]
helikon eval run  <dataset.jsonl> --agent NAME [--agents agents.toml] [--json] [--fail-under F] [--trace sqlite:PATH]
helikon mcp serve --agent NAME [--agents agents.toml] [--http ADDR]
```

- `repl` — an interactive, hot-reloading REPL: edit `agents.toml` and the next turn picks up the change, no restart needed.
- `eval run` — runs a JSONL dataset against one sidecar agent through [`paigasus-helikon-evals`](https://crates.io/crates/paigasus-helikon-evals) and prints trajectory + final-response scores.
- `mcp serve` — exposes one sidecar agent as an MCP server (stdio by default, streamable HTTP via `--http`).

## Minimal `agents.toml`

```toml
[agents.triage]
description  = "Routes personal-finance questions"
instructions = "Classify the question and route it to the right specialist."
model        = { provider = "openai", id = "gpt-5-mini" }  # reads OPENAI_API_KEY
tools        = ["lookup_spending"]

[tools.lookup_spending]
description = "Look up spending for a month"
params      = { type = "object", properties = { month = { type = "string" } }, required = ["month"] }
script      = "tools/lookup_spending.rhai"

[eval]
evaluators = ["exact_match", "tool_trajectory"]
```

`model.provider` is `"openai"`, `"anthropic"`, or `"mock"` (a recorded script, for deterministic/offline agents). Tools are sandboxed [Rhai](https://rhai.rs) scripts exposing `fn run(args)`.

See the [CLI reference](https://smk1085.github.io/paigasus-helikon/reference/cli.html) for the full flag grammar, a complete sidecar example, REPL slash commands, and hot-reload semantics.

## Links

- [Guide: CLI reference](https://smk1085.github.io/paigasus-helikon/reference/cli.html)
- [Crate roster](https://smk1085.github.io/paigasus-helikon/reference/crates.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.

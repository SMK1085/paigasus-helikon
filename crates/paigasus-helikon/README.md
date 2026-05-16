# paigasus-helikon

Facade crate for the Paigasus Helikon AI SDK. Re-exports `paigasus-helikon-core` plus feature-gated providers, runtimes, and extensions.

## Usage

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "anthropic", "mcp", "runtime-tokio"] }
```

> Pre-release: the workspace currently pins `version = "0.0.0"` and is not yet published to crates.io. The `"0.1"` shown above is the planned first published release — replace with the actual published version once available.

See [SMA-304](https://linear.app/smaschek/issue/SMA-304) for the bootstrap status.

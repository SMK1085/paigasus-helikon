# paigasus-helikon

Facade crate for the Paigasus Helikon AI SDK. Re-exports `paigasus-helikon-core` plus feature-gated providers, runtimes, and extensions.

## Usage

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "anthropic", "mcp", "runtime-tokio"] }
```

See [SMA-304](https://linear.app/smaschek/issue/SMA-304) for the bootstrap status.

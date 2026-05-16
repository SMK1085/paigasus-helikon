# paigasus-helikon

Paigasus AI SDK — codename **Helikon**. A Rust SDK for building AI agents with pluggable providers, runtimes, and tools.

This repository hosts the Cargo workspace. Add the SDK to a downstream project with:

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "anthropic", "mcp", "runtime-tokio"] }
```

Crates are versioned together. See `crates/` for the workspace layout.

## License

MIT — see [LICENSE](./LICENSE).

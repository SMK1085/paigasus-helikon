# Benchmarks

## `tool_dispatch` — `Tool::invoke` dispatch overhead

**What it measures.** The hot path an agent takes to invoke a tool: name-lookup
in a `Vec<Arc<dyn Tool<Ctx>>>` registry, the `dyn Tool` vtable call to `invoke`,
awaiting the returned future, and reading the JSON `ToolOutput`.

**Methodology.** A dependency-free `harness = false` bench (no Criterion). A
single `tokio` `block_on` wraps a warmup loop and the measured loop, so runtime
entry is amortized across all iterations rather than charged per call; the
measured loop is timed with `std::time::Instant` and divided by the iteration
count. `std::hint::black_box` prevents the optimizer eliding the work.

Criterion was deliberately avoided: its transitive `clap_lex 1.1.0` uses
`edition2024`, which Cargo 1.75 cannot parse — adding it would break the
workspace's Rust 1.75 MSRV.

**What it does NOT measure.** Network/provider latency, model invocation, or the
full agent loop — only `Tool::invoke` dispatch.

**Target.** < 50 µs. Deliberately loose: the lookup + vtable call + JSON read
should cost on the order of sub-µs, so 50 µs is ~50× headroom — only a
pathological regression trips it. This is a guard, not a tracked SLO (there is
no stored baseline; the bench `assert!`s the target and prints the number).

**Run it.**

```bash
cargo bench -p paigasus-helikon --bench tool_dispatch
```

No extra toolchain or dependencies needed. The bench is excluded from
`cargo test` via `[[bench]] test = false`.

## Results

Authoritative numbers are taken on **Linux x86_64** via the manual `bench.yml`
GitHub Actions job (`workflow_dispatch`). Local macOS/arm64 figures are
indicative only (~0.1 µs on an Apple-silicon dev box).

| Date | Platform | Runner | `tool_dispatch` | Target |
|---|---|---|---|---|
| 2026-06-02 | Linux x86_64 | GitHub `ubuntu-latest` | **178 ns/call** (200k iters) | < 50 µs |

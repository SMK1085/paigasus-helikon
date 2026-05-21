# Core Primitives

The seven object-safe traits — `Model`, `Tool<Ctx>`, `Agent<Ctx>`, `Session`, `Guardrail<Ctx>`, `Hook<Ctx>`, `Runner<Ctx>` — and the concrete carrier types they share.

The trait surface lives in the [`paigasus-helikon-core`](https://github.com/SMK1085/paigasus-helikon/tree/main/crates/paigasus-helikon-core) crate. Each trait carries a worked rustdoc example. Until the workspace publishes to crates.io, the source itself is the canonical reference; rustdoc HTML will become available on docs.rs after the first published release.

The seven traits were chosen as the minimum viable surface. Other primitives users may expect — `Memory`, `KnowledgeBase`, `Toolset`, `Plugin` — are either compositions of these seven (e.g. a `Toolset` is a function returning `Vec<Arc<dyn Tool<Ctx>>>`) or premature.

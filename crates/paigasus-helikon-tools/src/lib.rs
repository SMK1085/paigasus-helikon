//! Sandboxed filesystem and process tools for the Paigasus Helikon AI SDK.
//!
//! This crate provides agent [`Tool`](paigasus_helikon_core::Tool)s that
//! operate inside a `Sandbox` — a directory opened as an OS-confined
//! capability (`cap-std`), so `ReadTool`, `WriteTool`, and `EditTool`
//! cannot escape it via `..`, absolute paths, or symlinks.
//!
//! # Containment and `BashTool`
//!
//! `BashTool` runs commands through a pluggable [`ExecutionBackend`], so its
//! containment depends on the backend it is given. [`HostBackend`] (the default)
//! is a cwd-pinned shell and **NOT a security boundary** — the `cap-std`
//! containment that jails the filesystem tools does **not** extend to a spawned
//! child process, which can read and write anything this process can (absolute
//! paths, `..`, `~`, and the network). With no
//! [`PermissionPolicy`](paigasus_helikon_core::PermissionPolicy) installed the
//! control layer is permissive, so a host-backed `BashTool` runs **ungated** —
//! pair it with a `PermissionPolicy` or `DenyRule::tool("Bash")`. The
//! `OsSandboxBackend` (Linux, behind the `os-sandbox` feature) instead enforces
//! filesystem and syscall containment at the OS layer. Each backend reports what
//! it enforces via [`ExecutionBackend::guarantees`], surfaced in the tool's
//! description.

mod bash;
mod edit;
mod exec;
mod read;
mod sandbox;
mod write;

#[cfg(feature = "web")]
mod web;

pub use bash::{BashTool, BashToolBuilder};
pub use edit::EditTool;
pub use exec::{
    ExecOutput, ExecRequest, ExecutionBackend, HostBackend, HostBackendBuilder, Isolation,
    ResourceLimits, SandboxGuarantees,
};
#[cfg(all(feature = "os-sandbox", target_os = "linux"))]
pub use exec::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};
pub use read::ReadTool;
pub use sandbox::{Sandbox, SandboxError};
pub use write::WriteTool;

#[cfg(feature = "web")]
pub use web::{
    BraveBackend, SearchBackend, SearchResult, TavilyBackend, WebFetchTool, WebFetchToolBuilder,
    WebSearchTool, WebSearchToolBuilder,
};

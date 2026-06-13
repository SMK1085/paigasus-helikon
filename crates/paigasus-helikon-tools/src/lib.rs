//! Sandboxed filesystem and process tools for the Paigasus Helikon AI SDK.
//!
//! This crate provides agent [`Tool`](paigasus_helikon_core::Tool)s that
//! operate inside a `Sandbox` — a directory opened as an OS-confined
//! capability (`cap-std`), so `ReadTool`, `WriteTool`, and `EditTool`
//! cannot escape it via `..`, absolute paths, or symlinks.
//!
//! # Security note on `BashTool`
//!
//! `BashTool` is a **cwd-pinned shell, NOT a security sandbox.** The
//! `cap-std` containment that jails the filesystem tools does **not** extend to
//! a spawned child process: a command can read and write anything this process
//! can — absolute paths, `..`, `~`, and the network. In
//! [`PermissionMode::Default`](paigasus_helikon_core::PermissionMode) with no
//! [`PermissionPolicy`](paigasus_helikon_core::PermissionPolicy) installed, the
//! control layer is permissive, so `BashTool` runs **ungated**. Pair it with a
//! `PermissionPolicy` or `DenyRule::tool("Bash")` for real control.

mod edit;
mod read;
mod sandbox;
mod write;

pub use edit::EditTool;
pub use read::ReadTool;
pub use sandbox::{Sandbox, SandboxError};
pub use write::WriteTool;

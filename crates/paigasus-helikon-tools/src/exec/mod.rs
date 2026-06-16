//! The pluggable [`ExecutionBackend`] that [`crate::BashTool`] runs against, and
//! the shared types describing a backend's *containment* (distinct from the
//! runner's *approval* policy and from resource-capping).

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

// mod host;
// pub use host::{HostBackend, HostBackendBuilder};

// #[cfg(all(feature = "os-sandbox", target_os = "linux"))]
// mod os_sandbox;
// #[cfg(all(feature = "os-sandbox", target_os = "linux"))]
// pub use os_sandbox::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};

/// Default wall-clock timeout for a command (matches the SMA-328 `BashTool`).
// Unused until Task 3 (HostBackend); allow to keep Task 1 clippy-clean.
#[allow(dead_code)]
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default per-stream output cap, in bytes (1 MiB).
// Unused until Task 3 (HostBackend); allow to keep Task 1 clippy-clean.
#[allow(dead_code)]
pub const DEFAULT_MAX_OUTPUT: usize = 1 << 20;

/// A backend that runs one shell command under some containment tier.
///
/// Object-safe and not generic over the agent context, so one value is shared as
/// `Arc<dyn ExecutionBackend>` across agents of any context type. Swapping the
/// backend needs no change to [`crate::BashTool`] or agent code.
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Run `req.command` to completion under this backend's containment.
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError>;

    /// What this backend actually enforces — surfaced in docs, the model-facing
    /// tool description, and traces. Describes *containment*, not approval.
    fn guarantees(&self) -> SandboxGuarantees;
}

/// A request to run one shell command. `#[non_exhaustive]` so per-call knobs
/// (stdin, env overrides) can be added later without a breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecRequest {
    /// The shell command (`sh -c <command>` on unix, `cmd /C` on Windows).
    pub command: String,
}

impl ExecRequest {
    /// Build a request from a command string.
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
        }
    }
}

/// The captured result of one command run. Non-zero exit, timeout, and
/// truncation are *normal results* the model inspects — never errors.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecOutput {
    /// Captured stdout (lossy UTF-8, truncated at the backend's output cap).
    pub stdout: String,
    /// Captured stderr (lossy UTF-8, truncated at the backend's output cap).
    pub stderr: String,
    /// Process exit code, or `None` if killed by signal / timeout.
    pub exit_code: Option<i32>,
    /// Whether the command was killed because it exceeded the timeout.
    pub timed_out: bool,
    /// Whether either stream was truncated at the output cap.
    pub truncated: bool,
}

impl ExecOutput {
    /// Construct an `ExecOutput` from all fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        stdout: String,
        stderr: String,
        exit_code: Option<i32>,
        timed_out: bool,
        truncated: bool,
    ) -> Self {
        Self {
            stdout,
            stderr,
            exit_code,
            timed_out,
            truncated,
        }
    }
}

/// The isolation level enforced on one axis (filesystem / network / syscalls).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Isolation {
    /// No OS enforcement — the command has the same access this process does.
    None,
    /// Enforced by an OS kernel mechanism (Landlock / seccomp-bpf).
    OsKernel,
}

/// What a backend enforces, surfaced to docs / traces / the tool description.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SandboxGuarantees {
    /// Filesystem containment.
    pub filesystem: Isolation,
    /// Network containment.
    pub network: Isolation,
    /// Syscall containment.
    pub syscalls: Isolation,
    /// Short human label, e.g. `"host (no containment)"`.
    pub label: &'static str,
}

impl SandboxGuarantees {
    /// Construct a `SandboxGuarantees` from all axes.
    pub fn new(
        filesystem: Isolation,
        network: Isolation,
        syscalls: Isolation,
        label: &'static str,
    ) -> Self {
        Self {
            filesystem,
            network,
            syscalls,
            label,
        }
    }
}

/// Resource limits applied to a command via `setrlimit` (unix). Each `None`
/// leaves the inherited limit. See `HostBackend` for the default policy.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ResourceLimits {
    /// `RLIMIT_CPU` — CPU seconds.
    pub cpu_seconds: Option<u64>,
    /// `RLIMIT_FSIZE` — max bytes a single file write may reach.
    pub file_size_bytes: Option<u64>,
    /// `RLIMIT_AS` — virtual address-space cap. Approximate; off by default
    /// because it spuriously kills threaded/`mmap`-heavy programs.
    pub address_space_bytes: Option<u64>,
}

/// Internal config every backend shares; consumed by `spawn_capped`.
// Unused until Task 2 (spawn_capped); allow to keep Task 1 clippy-clean.
#[allow(dead_code)]
pub(crate) struct ExecConfig {
    pub(crate) cwd: PathBuf,
    pub(crate) env_allowlist: Vec<String>,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: usize,
}

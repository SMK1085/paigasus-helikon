//! The pluggable [`ExecutionBackend`] that [`crate::BashTool`] runs against, and
//! the shared types describing a backend's *containment* (distinct from the
//! runner's *approval* policy and from resource-capping).

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

mod host;
pub use host::{HostBackend, HostBackendBuilder};

#[cfg(all(
    feature = "os-sandbox",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod os_sandbox;
#[cfg(all(
    feature = "os-sandbox",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use os_sandbox::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};

#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
mod os_sandbox_seatbelt;
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
pub use os_sandbox_seatbelt::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};

#[cfg(feature = "microvm")]
mod forkd;
#[cfg(feature = "microvm")]
/// forkd microVM backend types.
pub use forkd::{ForkdBackend, ForkdBackendBuilder, ForkdError, ReconcileReport};

/// Default wall-clock timeout for a command (matches the SMA-328 `BashTool`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default per-stream output cap, in bytes (1 MiB).
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
    /// Isolated by a hardware-virtualization (KVM/hypervisor) boundary — a
    /// separate guest kernel. `Virtualized` means the whole machine is isolated,
    /// **not** that any one axis is filtered: a microVM does not filter syscalls
    /// the way `OsKernel` (seccomp) does — the guest issues syscalls to its own
    /// kernel. Stronger overall than `OsKernel`, but read each axis as "behind a
    /// VM boundary," not "restricted by an allowlist."
    Virtualized,
    /// Egress is filtered by an allow/deny **domain** policy at a CONNECT/HTTP
    /// proxy (application layer). `Proxied` is meaningful **only in the layered
    /// deployment**: a per-VM netns default-deny that drops all egress except the
    /// proxy path (and DNS to a vetted resolver). Without that L3/L4 default-deny,
    /// non-proxy-aware clients, DNS (UDP/53), QUIC/HTTP-3 (UDP/443), and raw TCP
    /// **escape** — the proxy never sees them. The backend cannot verify the host's
    /// netns rules, so this tier reflects an operator attestation (see
    /// `ForkdBackendBuilder::enforce_egress`), the same trust model the other tiers
    /// apply to the kernel/hypervisor. Read as "HTTP/S egress is domain-filtered,
    /// given the netns default-deny," not "all packets are blocked."
    Proxied,
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
///
/// Not `#[non_exhaustive]`: this is user-built config (struct-literal
/// construction from consumer code and integration tests); adding a field later
/// is an accepted 0.x breaking change.
#[derive(Debug, Clone, Default)]
pub struct ResourceLimits {
    /// `RLIMIT_CPU` — CPU seconds.
    pub cpu_seconds: Option<u64>,
    /// `RLIMIT_FSIZE` — max bytes a single file write may reach.
    pub file_size_bytes: Option<u64>,
    /// `RLIMIT_AS` — virtual address-space cap. Approximate; off by default
    /// because it spuriously kills threaded/`mmap`-heavy programs.
    pub address_space_bytes: Option<u64>,
}

/// Internal config every backend shares; consumed by [`spawn_capped`].
pub(crate) struct ExecConfig {
    pub(crate) cwd: PathBuf,
    pub(crate) env_allowlist: Vec<String>,
    pub(crate) timeout: Duration,
    pub(crate) max_output_bytes: usize,
}

use std::process::Stdio;
use tokio::io::AsyncReadExt;

/// Grace period for reaping a killed process and draining its pipes.
const GRACE: Duration = Duration::from_secs(5);

/// Spawn `command` under `cfg`, draining stdout/stderr concurrently, killing the
/// whole process group on timeout. `prefix`, when non-empty, is prepended as
/// `program [args...]` ahead of `sh -c <command>` (used by the macOS Seatbelt
/// backend to wrap the shell in `sandbox-exec`). `configure_child` runs in the
/// **parent** to install backend-specific `pre_exec` hooks before spawn.
pub(crate) async fn spawn_capped(
    cfg: &ExecConfig,
    prefix: &[OsString],
    command: &str,
    configure_child: impl FnOnce(&mut tokio::process::Command),
) -> Result<ExecOutput, ToolError> {
    let mut cmd = build_command(prefix, command);
    cmd.current_dir(&cfg.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in &cfg.env_allowlist {
        if let Ok(val) = std::env::var(name) {
            cmd.env(name, val);
        }
    }
    #[cfg(unix)]
    {
        // New process group so a timeout can kill the whole subtree.
        cmd.process_group(0);
    }
    configure_child(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| ToolError::Other(anyhow::anyhow!("failed to spawn shell: {e}")))?;

    #[cfg(unix)]
    let pgid = child.id();

    let stdout_pipe = child.stdout.take().expect("piped stdout");
    let stderr_pipe = child.stderr.take().expect("piped stderr");

    let cap = cfg.max_output_bytes;
    let out_handle = tokio::spawn(read_capped(stdout_pipe, cap));
    let err_handle = tokio::spawn(read_capped(stderr_pipe, cap));

    let mut timed_out = false;
    let exit_code: Option<i32>;
    match tokio::time::timeout(cfg.timeout, child.wait()).await {
        Ok(status) => {
            exit_code = status.map_err(|e| ToolError::Other(e.into()))?.code();
        }
        Err(_) => {
            timed_out = true;
            #[cfg(unix)]
            {
                if let Some(pid) = pgid {
                    // SAFETY: pid < 4_194_304 on supported platforms, so the cast
                    // and negation are valid; ESRCH (group already gone) is benign.
                    let _ = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.start_kill();
            }
            exit_code = match tokio::time::timeout(GRACE, child.wait()).await {
                Ok(Ok(status)) => status.code(),
                _ => None,
            };
        }
    }

    let ((stdout, out_trunc), (stderr, err_trunc)) =
        tokio::join!(join_reader(out_handle), join_reader(err_handle));

    Ok(ExecOutput {
        stdout,
        stderr,
        exit_code,
        timed_out,
        truncated: out_trunc || err_trunc,
    })
}

/// Build the platform shell command without yet setting cwd/env. `prefix`, when
/// non-empty, is prepended as `program [args...]` before `sh -c <command>` — used
/// by the macOS Seatbelt backend to wrap the shell in `sandbox-exec`. Empty for
/// the host and Linux backends.
fn build_command(prefix: &[OsString], command: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        match prefix.split_first() {
            Some((program, rest)) => {
                let mut c = tokio::process::Command::new(program);
                c.args(rest).arg("sh").arg("-c").arg(command);
                c
            }
            None => {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(command);
                c
            }
        }
    }
    #[cfg(windows)]
    {
        // `prefix` is always empty on Windows; bind it to avoid `unused_variables`
        // under `-D warnings` (clippy runs on ubuntu; Windows is signal-only).
        let _ = prefix;
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
}

async fn join_reader(handle: tokio::task::JoinHandle<(String, bool)>) -> (String, bool) {
    let abort = handle.abort_handle();
    match tokio::time::timeout(GRACE, handle).await {
        Ok(Ok(captured)) => captured,
        _ => {
            abort.abort();
            (String::new(), false)
        }
    }
}

async fn read_capped<R>(pipe: R, cap: usize) -> (String, bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut buf = Vec::new();
    let _ = pipe.take((cap as u64) + 1).read_to_end(&mut buf).await;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

/// Apply `limits` to the current process via `setrlimit`. Async-signal-safe
/// (only `setrlimit` syscalls, no allocation) so it is callable from `pre_exec`.
#[cfg(unix)]
pub(crate) fn apply_rlimits(limits: &ResourceLimits) -> std::io::Result<()> {
    // SAFETY: setrlimit with a stack-allocated rlimit is async-signal-safe.
    unsafe {
        // The resource arg's type differs per target (`c_int` on macOS/BSD/musl,
        // `__rlimit_resource_t` on Linux glibc), so it is inferred from the
        // platform's `RLIMIT_*` constants rather than fixed to one type.
        let set = |res, val: u64| -> std::io::Result<()> {
            let rl = libc::rlimit {
                rlim_cur: val as libc::rlim_t,
                rlim_max: val as libc::rlim_t,
            };
            if libc::setrlimit(res, &rl) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        };
        if let Some(s) = limits.cpu_seconds {
            set(libc::RLIMIT_CPU, s)?;
        }
        if let Some(b) = limits.file_size_bytes {
            set(libc::RLIMIT_FSIZE, b)?;
        }
        if let Some(b) = limits.address_space_bytes {
            set(libc::RLIMIT_AS, b)?;
        }
    }
    Ok(())
}

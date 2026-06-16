//! [`OsSandboxBackend`] — OS-enforced Bash containment on Linux via Landlock
//! (filesystem) + seccomp-bpf (syscalls / network). Fail-closed: `build()` errors
//! if the kernel cannot enforce the requested isolation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use landlock::{
    path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, ABI,
};
use paigasus_helikon_core::ToolError;
use seccompiler::{
    apply_filter, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

use super::{
    spawn_capped, ExecConfig, ExecOutput, ExecRequest, ExecutionBackend, Isolation, ResourceLimits,
    SandboxGuarantees, DEFAULT_MAX_OUTPUT, DEFAULT_TIMEOUT,
};
use crate::sandbox::Sandbox;

/// Landlock ABI floor we require for filesystem containment (kernel ≥ 5.13).
const LANDLOCK_ABI: ABI = ABI::V1;

/// Target arch for the seccomp BPF. The `os_sandbox` module is gated to these two
/// arches (see `super::os_sandbox` cfg), so exactly one of these is compiled.
#[cfg(target_arch = "x86_64")]
const SECCOMP_ARCH: TargetArch = TargetArch::x86_64;
#[cfg(target_arch = "aarch64")]
const SECCOMP_ARCH: TargetArch = TargetArch::aarch64;

/// Construction failures for [`OsSandboxBackend`]. Distinct from
/// `ToolError::Denied` (an in-`run` refusal) and from `SandboxError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OsSandboxError {
    /// The kernel cannot enforce Landlock at the required ABI.
    #[error("OS sandbox unavailable: {0}")]
    Unsupported(String),
}

/// Builder for [`OsSandboxBackend`].
pub struct OsSandboxBackendBuilder {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    limits: ResourceLimits,
    allow_network: bool,
    read_paths: Vec<PathBuf>,
}

impl OsSandboxBackendBuilder {
    /// Wall-clock timeout before the process group is killed (default 30s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Env var names to pass through (REPLACES the default `["PATH","HOME"]`).
    pub fn env_allowlist<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env_allowlist = names.into_iter().map(Into::into).collect();
        self
    }
    /// Truncate captured stdout/stderr to this many bytes each (default 1 MiB).
    pub fn max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n;
        self
    }
    /// Override resource limits applied inside the jail.
    pub fn rlimits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }
    /// Allow outbound network (default: deny all IP egress).
    pub fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }
    /// Extra read-only path exceptions beyond the default system set.
    pub fn read_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.read_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Finish building. **Fail-closed:** returns `Err` if Landlock cannot be
    /// enforced at [`LANDLOCK_ABI`] on this kernel.
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, OsSandboxError> {
        probe_landlock()?;
        Ok(Arc::new(OsSandboxBackend {
            cfg: ExecConfig {
                cwd: self.sandbox.root().to_path_buf(),
                env_allowlist: self.env_allowlist,
                timeout: self.timeout,
                max_output_bytes: self.max_output_bytes,
            },
            limits: self.limits,
            allow_network: self.allow_network,
            root: self.sandbox.root().to_path_buf(),
            read_paths: self.read_paths,
        }))
    }
}

/// Probe Landlock support without restricting the current process: create a
/// ruleset fd under a hard-requirement compat level and drop it.
fn probe_landlock() -> Result<(), OsSandboxError> {
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(LANDLOCK_ABI))
        .and_then(|r| r.create())
        .map(|_created| ())
        .map_err(|e| OsSandboxError::Unsupported(e.to_string()))
}

/// Read-only system paths a shell + common tools need. (`/usr/sbin` is covered
/// by `/usr`; `/sbin` is listed for distros that have not usr-merged it.)
const SYSTEM_RO: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc"];

/// Build (in the parent) a Landlock ruleset: read+write under the sandbox root,
/// read+exec for the system paths and any extra `read_paths`.
fn build_ruleset(
    root: &std::path::Path,
    read_paths: &[PathBuf],
) -> Result<landlock::RulesetCreated, landlock::RulesetError> {
    let abi = LANDLOCK_ABI;
    let ro: Vec<PathBuf> = SYSTEM_RO
        .iter()
        .map(PathBuf::from)
        .chain(read_paths.iter().cloned())
        .filter(|p| p.exists())
        .collect();
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules(&ro, AccessFs::from_read(abi)))?
        .add_rules(path_beneath_rules([root], AccessFs::from_all(abi)))
}

/// Compile (in the parent) a seccomp filter: allow by default, return EPERM for a
/// dangerous syscall set; when `allow_network` is false also EPERM `socket()` for
/// `AF_INET`/`AF_INET6` (`AF_UNIX` stays allowed).
fn build_seccomp(allow_network: bool) -> Result<BpfProgram, ToolError> {
    // The `backend` constructors (`SeccompCondition/Rule/Filter::new`, the
    // `BpfProgram` `TryFrom`) all surface `seccompiler::BackendError`, NOT the
    // top-level `seccompiler::Error` (that one is for `apply_filter`).
    let err = |e: seccompiler::BackendError| ToolError::Other(anyhow::anyhow!("seccomp: {e}"));
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();

    // Always-deny dangerous syscalls (empty rule vec = match regardless of args).
    for sc in [
        libc::SYS_ptrace,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_kexec_load,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
    ] {
        rules.insert(sc, vec![]);
    }

    if !allow_network {
        // Match `socket(family, ..)` for AF_INET / AF_INET6 (arg0); AF_UNIX is
        // left allowed so local IPC and `/dev/log`-style sockets keep working.
        // x86_64/aarch64 use the direct `socket` syscall, so this is the egress
        // chokepoint. (The x32 ABI ORs a bit into the syscall number and would
        // slip past, but standard amd64 userland — including CI — ships no x32
        // binaries, so it is out of scope.)
        let af = |family: u64| -> Result<SeccompRule, ToolError> {
            SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Dword,
                SeccompCmpOp::Eq,
                family,
            )
            .map_err(err)?])
            .map_err(err)
        };
        rules.insert(
            libc::SYS_socket,
            vec![af(libc::AF_INET as u64)?, af(libc::AF_INET6 as u64)?],
        );
    }

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,                     // default: allow
        SeccompAction::Errno(libc::EPERM as u32), // on match: EPERM
        SECCOMP_ARCH,
    )
    .map_err(err)?;
    filter.try_into().map_err(err)
}

/// OS-enforced execution backend (Linux). See module docs.
pub struct OsSandboxBackend {
    cfg: ExecConfig,
    limits: ResourceLimits,
    allow_network: bool,
    root: PathBuf,
    read_paths: Vec<PathBuf>,
}

impl OsSandboxBackend {
    /// Start building over `sandbox` (read+write root; default deny-network).
    pub fn builder(sandbox: Sandbox) -> OsSandboxBackendBuilder {
        OsSandboxBackendBuilder {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            env_allowlist: vec!["PATH".to_owned(), "HOME".to_owned()],
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            limits: ResourceLimits::default(),
            allow_network: false,
            read_paths: Vec::new(),
        }
    }
}

#[async_trait]
impl ExecutionBackend for OsSandboxBackend {
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        let limits = self.limits.clone();
        // Built in the PARENT (allocations are safe here, not in the child).
        let ruleset = build_ruleset(&self.root, &self.read_paths)
            .map_err(|e| ToolError::Other(anyhow::anyhow!("landlock ruleset: {e}")))?;
        let mut ruleset = Some(ruleset);
        // Compile the seccomp BPF in the PARENT too (BTreeMap/Vec allocation is
        // not async-signal-safe and must not happen in the forked child).
        let seccomp = build_seccomp(self.allow_network)?;

        spawn_capped(&self.cfg, &req.command, move |cmd| {
            // SAFETY: the closure runs in the forked child before exec, so it does
            // only async-signal-safe work — no heap allocation, no locks: the
            // `setrlimit` syscalls in `apply_rlimits`, then Landlock's
            // `restrict_self` (`prctl(PR_SET_NO_NEW_PRIVS)` + `landlock_restrict_self`
            // on an already-created ruleset fd, plus a small stack struct), then
            // `apply_filter` (a `prctl(PR_SET_SECCOMP)` syscall over the pre-compiled
            // BPF). The `RulesetCreated` and `BpfProgram` are built in the parent and
            // moved in; the ruleset is taken via `Option::take` so it applies once.
            unsafe {
                cmd.pre_exec(move || {
                    super::apply_rlimits(&limits)?;
                    if let Some(rs) = ruleset.take() {
                        let status = rs
                            .restrict_self()
                            .map_err(|e| std::io::Error::other(e.to_string()))?;
                        // Fail-closed: accept only full enforcement. Anything less
                        // (a future ABI making Partial reachable) aborts the exec
                        // rather than running under weaker containment than claimed.
                        if status.ruleset != RulesetStatus::FullyEnforced {
                            return Err(std::io::Error::other("landlock not fully enforced"));
                        }
                    }
                    // Applied after Landlock. An unprivileged seccomp install needs
                    // PR_SET_NO_NEW_PRIVS, which `restrict_self` sets — though
                    // `apply_filter` also sets it itself, so this is not a strict
                    // seccomp prerequisite; we order Landlock first for its own
                    // semantics.
                    apply_filter(&seccomp).map_err(|e| std::io::Error::other(e.to_string()))?;
                    Ok(())
                });
            }
        })
        .await
    }

    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees {
            filesystem: Isolation::OsKernel,
            network: if self.allow_network {
                Isolation::None
            } else {
                Isolation::OsKernel
            },
            syscalls: Isolation::OsKernel,
            label: "os-sandbox (landlock+seccomp)",
        }
    }
}

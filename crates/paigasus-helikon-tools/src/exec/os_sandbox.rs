//! [`OsSandboxBackend`] — OS-enforced Bash containment on Linux via Landlock
//! (filesystem) + seccomp-bpf (syscalls / network). Fail-closed: `build()` errors
//! if the kernel cannot enforce the requested isolation.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use landlock::{Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, ABI};
use paigasus_helikon_core::ToolError;

use super::{
    spawn_capped, ExecConfig, ExecOutput, ExecRequest, ExecutionBackend, Isolation, ResourceLimits,
    SandboxGuarantees, DEFAULT_MAX_OUTPUT, DEFAULT_TIMEOUT,
};
use crate::sandbox::Sandbox;

/// Landlock ABI floor we require for filesystem containment (kernel ≥ 5.13).
const LANDLOCK_ABI: ABI = ABI::V1;

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

/// OS-enforced execution backend (Linux). See module docs.
pub struct OsSandboxBackend {
    cfg: ExecConfig,
    limits: ResourceLimits,
    allow_network: bool,
    #[allow(dead_code)] // wired into the Landlock ruleset in Task 7
    root: PathBuf,
    #[allow(dead_code)] // wired into the Landlock ruleset in Task 7
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
        // Task 7 adds the Landlock ruleset; Task 8 adds the seccomp filter.
        spawn_capped(&self.cfg, &req.command, move |cmd| {
            // SAFETY: apply_rlimits is async-signal-safe.
            unsafe {
                cmd.pre_exec(move || super::apply_rlimits(&limits));
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

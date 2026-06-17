//! [`HostBackend`] — the default execution backend. A cwd-pinned shell with env
//! scrubbing, an output cap, a timeout (process-group kill), and `rlimit`s.
//! **NOT a security boundary:** a spawned command can read/write anything this
//! process can. Gate it with a `PermissionPolicy` or use [`OsSandboxBackend`]
//! for OS-enforced containment.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

use super::{
    spawn_capped, ExecConfig, ExecOutput, ExecRequest, ExecutionBackend, Isolation, ResourceLimits,
    SandboxGuarantees, DEFAULT_MAX_OUTPUT, DEFAULT_TIMEOUT,
};
use crate::sandbox::Sandbox;

/// Default `RLIMIT_FSIZE` cap: 1 GiB.
const DEFAULT_FILE_SIZE_LIMIT: u64 = 1 << 30;
/// Extra CPU seconds granted over the wall timeout for the `RLIMIT_CPU` backstop.
const CPU_LIMIT_MARGIN_SECS: u64 = 5;

/// Builder for [`HostBackend`].
pub struct HostBackendBuilder {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    limits: ResourceLimits,
    limits_set: bool,
}

impl HostBackendBuilder {
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

    /// Override the resource limits. Replaces the defaults
    /// (`RLIMIT_CPU` = timeout+5s, `RLIMIT_FSIZE` = 1 GiB, `RLIMIT_AS` = unset).
    pub fn rlimits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self.limits_set = true;
        self
    }

    /// Finish building, returning a shareable `Arc<dyn ExecutionBackend>`.
    pub fn build(mut self) -> Arc<dyn ExecutionBackend> {
        if !self.limits_set {
            // Default policy: CPU backstop derived from the timeout, a generous
            // file-size cap, address-space cap left off (review M2).
            self.limits = ResourceLimits {
                cpu_seconds: Some(self.timeout.as_secs().saturating_add(CPU_LIMIT_MARGIN_SECS)),
                file_size_bytes: Some(DEFAULT_FILE_SIZE_LIMIT),
                address_space_bytes: None,
            };
        }
        Arc::new(HostBackend {
            cfg: ExecConfig {
                cwd: self.sandbox.root().to_path_buf(),
                env_allowlist: self.env_allowlist,
                timeout: self.timeout,
                max_output_bytes: self.max_output_bytes,
            },
            limits: self.limits,
        })
    }
}

/// The default, cwd-pinned execution backend. See the module docs: **not** a
/// security boundary.
pub struct HostBackend {
    cfg: ExecConfig,
    limits: ResourceLimits,
}

impl HostBackend {
    /// Start building a `HostBackend` over `sandbox` (cwd = `sandbox.root()`),
    /// with a 30s timeout, `["PATH","HOME"]` env allowlist, 1 MiB output cap.
    pub fn builder(sandbox: Sandbox) -> HostBackendBuilder {
        HostBackendBuilder {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            env_allowlist: vec!["PATH".to_owned(), "HOME".to_owned()],
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            limits: ResourceLimits::default(),
            limits_set: false,
        }
    }
}

#[async_trait]
impl ExecutionBackend for HostBackend {
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        let limits = self.limits.clone();
        spawn_capped(&self.cfg, &[], &req.command, move |_cmd| {
            #[cfg(unix)]
            {
                // SAFETY: apply_rlimits is async-signal-safe (setrlimit only).
                unsafe {
                    _cmd.pre_exec(move || super::apply_rlimits(&limits));
                }
            }
        })
        .await
    }

    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees {
            filesystem: Isolation::None,
            network: Isolation::None,
            syscalls: Isolation::None,
            label: "host (no containment)",
        }
    }
}

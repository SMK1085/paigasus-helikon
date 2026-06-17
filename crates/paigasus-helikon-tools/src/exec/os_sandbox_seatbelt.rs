//! [`OsSandboxBackend`] — OS-enforced Bash containment on macOS via Seatbelt
//! (the `sandbox-exec` profile sandbox). **Write-focused:** deny-by-default,
//! reads allowed (dyld needs the cryptex shared cache), read+write only within
//! the sandbox root, all-or-nothing network. Fail-closed: `build()` errors if the
//! profile cannot be applied.
//!
//! **Honesty (see the design spec §2.2/§2.3):** `guarantees().filesystem` is
//! `OsKernel` but enforces *write* containment only — reads are unrestricted,
//! weaker than the Linux backend which restricts reads too. `syscalls` is `None`:
//! Seatbelt is an operation-level MAC, not a syscall filter, and the profile
//! allows `process*`. Broad `mach-lookup`/`process*` are an accepted v1 residual
//! risk; deny-network also blocks `AF_UNIX` (stricter than Linux). `sandbox-exec`
//! is Apple-deprecated but ships on every macOS; the trait seam keeps an FFI swap
//! open later.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

use super::{
    spawn_capped, ExecConfig, ExecOutput, ExecRequest, ExecutionBackend, Isolation, ResourceLimits,
    SandboxGuarantees, DEFAULT_MAX_OUTPUT, DEFAULT_TIMEOUT,
};
use crate::sandbox::Sandbox;

/// Absolute path to the macOS Seatbelt wrapper (ships with the OS). Absolute so a
/// scrubbed `PATH` cannot hide it.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Construction failures for [`OsSandboxBackend`]. Distinct from
/// `ToolError::Denied` (an in-`run` refusal) and from `SandboxError`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OsSandboxError {
    /// Seatbelt could not be established: `sandbox-exec` missing, the profile was
    /// rejected, or the probe showed the sandbox did not contain a write.
    #[error("OS sandbox unavailable: {0}")]
    Unsupported(String),
}

/// Builder for [`OsSandboxBackend`]. Method-for-method parity with the Linux
/// builder so consumer code is identical across platforms.
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
    /// Override resource limits applied inside the jail (via `setrlimit`).
    pub fn rlimits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }
    /// Allow outbound network (default: deny all egress, including `AF_UNIX`).
    pub fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }
    /// **No-op on macOS.** The write-focused Seatbelt posture allows all reads, so
    /// extra read-only exceptions have no effect. Kept for API parity with the
    /// Linux backend so consumer code is identical across platforms.
    pub fn read_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.read_paths = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Finish building. **Fail-closed:** returns `Err` if Seatbelt cannot be
    /// established (no `sandbox-exec`, profile rejected, or the probe shows the
    /// sandbox did not contain a write inside the root).
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, OsSandboxError> {
        // `read_paths` is intentionally unused on macOS (see the builder method).
        let _ = self.read_paths;
        let root = self.sandbox.root().to_path_buf(); // already canonical (Sandbox::open)
        let profile = build_profile(self.allow_network);
        probe(&root, &profile)?;
        let prefix = wrapper_prefix(&root, &profile);
        Ok(Arc::new(OsSandboxBackend {
            cfg: ExecConfig {
                cwd: root,
                env_allowlist: self.env_allowlist,
                timeout: self.timeout,
                max_output_bytes: self.max_output_bytes,
            },
            limits: self.limits,
            allow_network: self.allow_network,
            prefix,
        }))
    }
}

/// Build the write-focused SBPL profile. `ROOT` is supplied separately via
/// `sandbox-exec -D ROOT=…` and referenced as `(param "ROOT")`, so no path text is
/// spliced into the profile (no SBPL-injection surface).
fn build_profile(allow_network: bool) -> String {
    let mut p = String::from(
        r#"(version 1)
(deny default)
(allow process*)
(allow sysctl-read)
(allow mach-lookup)
(allow file-read*)
(allow file-write*
  (subpath (param "ROOT"))
  (literal "/dev/null") (literal "/dev/stdout") (literal "/dev/stderr")
  (literal "/dev/tty") (literal "/dev/dtracehelper"))
"#,
    );
    if allow_network {
        p.push_str("(allow network*)\n");
    }
    p
}

/// The argv that must precede `sh -c <command>`: the Seatbelt wrapper with `ROOT`
/// bound and the profile inline.
fn wrapper_prefix(root: &Path, profile: &str) -> Vec<OsString> {
    vec![
        OsString::from(SANDBOX_EXEC),
        OsString::from("-D"),
        OsString::from(format!("ROOT={}", root.display())),
        OsString::from("-p"),
        OsString::from(profile),
    ]
}

/// Fail-closed probe: run a real shell under the profile that writes a file inside
/// the root, then removes it. Validates `sandbox-exec` exists, the profile
/// compiles, the shell starts under it, and the root write-rule actually permits
/// writes (catching a canonicalization/rule bug at construction). The marker path
/// is passed as `$1` (argv), never interpolated into the script. The
/// write-OUTSIDE-root denial is covered by the AC test (a required CI check).
fn probe(root: &Path, profile: &str) -> Result<(), OsSandboxError> {
    let marker = root.join(".helikon-seatbelt-probe");
    let out = std::process::Command::new(SANDBOX_EXEC)
        .arg("-D")
        .arg(format!("ROOT={}", root.display()))
        .arg("-p")
        .arg(profile)
        .arg("/bin/sh")
        .arg("-c")
        .arg(r#"echo ok > "$1" && rm -f "$1""#)
        .arg("seatbelt-probe") // $0
        .arg(&marker) // $1 — no shell interpolation
        .output()
        .map_err(|e| OsSandboxError::Unsupported(format!("cannot run {SANDBOX_EXEC}: {e}")))?;
    let _ = std::fs::remove_file(&marker);
    if !out.status.success() {
        return Err(OsSandboxError::Unsupported(format!(
            "Seatbelt probe failed (status {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// OS-enforced execution backend (macOS, Seatbelt). See module docs.
pub struct OsSandboxBackend {
    cfg: ExecConfig,
    limits: ResourceLimits,
    allow_network: bool,
    prefix: Vec<OsString>,
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
        // The jail is applied by `sandbox-exec` (the prefix); the only `pre_exec`
        // work is the shared, async-signal-safe `setrlimit`s.
        spawn_capped(&self.cfg, &self.prefix, &req.command, move |_cmd| {
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
            filesystem: Isolation::OsKernel, // write-containment only (see module docs)
            network: if self.allow_network {
                Isolation::None
            } else {
                Isolation::OsKernel
            },
            syscalls: Isolation::None,
            label: "os-sandbox (seatbelt)",
        }
    }
}

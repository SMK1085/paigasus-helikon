//! [`BashTool`] — a cwd-pinned shell. **NOT a security sandbox** (see the
//! crate-level docs): a spawned command can read/write anything this process
//! can. Gate it with a `PermissionPolicy` or `DenyRule::tool("Bash")`.

use std::marker::PhantomData;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::{Tool, ToolContext, ToolEffect, ToolError, ToolOutput};
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::sandbox::Sandbox;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_OUTPUT: usize = 1 << 20; // 1 MiB

/// Grace period for reaping a killed process and for draining its pipes after
/// the main timeout fires (or after a successful exit when a backgrounded
/// process still holds a pipe open).
const GRACE: Duration = Duration::from_secs(5);

/// Arguments for [`BashTool`].
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BashArgs {
    /// The shell command to run (`sh -c <command>` on unix, `cmd /C` on Windows).
    command: String,
}

/// Builder for [`BashTool`].
pub struct BashToolBuilder {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
}

impl BashToolBuilder {
    /// Maximum wall-clock duration before the command's process group (unix) or
    /// the child process (other platforms) is killed.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Environment variable names to pass through (the rest are dropped). This
    /// REPLACES the default `["PATH", "HOME"]` allowlist; include `"PATH"`
    /// explicitly if the command needs it.
    pub fn env_allowlist<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env_allowlist = names.into_iter().map(Into::into).collect();
        self
    }

    /// Truncate captured stdout/stderr to this many bytes each.
    pub fn max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n;
        self
    }

    /// Refuse any command whose first whitespace-delimited token matches one of
    /// these names.
    pub fn deny_commands<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny_commands = names.into_iter().map(Into::into).collect();
        self
    }

    /// If set, allow ONLY commands whose first token is in this list.
    pub fn allow_commands<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_commands = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// Finish building.
    pub fn build<Ctx>(self) -> BashTool<Ctx> {
        BashTool {
            sandbox: self.sandbox,
            timeout: self.timeout,
            env_allowlist: self.env_allowlist,
            max_output_bytes: self.max_output_bytes,
            deny_commands: self.deny_commands,
            allow_commands: self.allow_commands,
            schema: serde_json::to_value(schemars::schema_for!(BashArgs))
                .expect("BashArgs schema serializes"),
            _ctx: PhantomData,
        }
    }
}

/// A cwd-pinned shell tool. See the crate-level security note: this is **not**
/// a security boundary.
pub struct BashTool<Ctx = ()> {
    sandbox: Sandbox,
    timeout: Duration,
    env_allowlist: Vec<String>,
    max_output_bytes: usize,
    deny_commands: Vec<String>,
    allow_commands: Option<Vec<String>>,
    schema: Value,
    _ctx: PhantomData<fn() -> Ctx>,
}

impl BashTool<()> {
    /// Start building a `BashTool` over `sandbox` (cwd = `sandbox.root()`),
    /// with a 30s timeout, a `["PATH", "HOME"]` env allowlist, and a 1 MiB
    /// output cap.
    pub fn builder(sandbox: Sandbox) -> BashToolBuilder {
        BashToolBuilder {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            env_allowlist: vec!["PATH".to_owned(), "HOME".to_owned()],
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            deny_commands: Vec::new(),
            allow_commands: None,
        }
    }
}

impl<Ctx> BashTool<Ctx> {
    fn check_command_allowed(&self, command: &str) -> Result<(), ToolError> {
        let first = command.split_whitespace().next().unwrap_or("");
        if self.deny_commands.iter().any(|d| d == first) {
            return Err(ToolError::Denied {
                reason: format!("command `{first}` is blocked by the deny list"),
            });
        }
        if let Some(allow) = &self.allow_commands {
            if !allow.iter().any(|a| a == first) {
                return Err(ToolError::Denied {
                    reason: format!("command `{first}` is not in the allow list"),
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl<Ctx> Tool<Ctx> for BashTool<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Run a shell command with the working directory pinned to the sandbox \
         root. WARNING: this is NOT a security sandbox — the command can read/ \
         write anything this process can (absolute paths, the network). It is \
         ungated unless a PermissionPolicy or a DenyRule for `Bash` is installed."
    }

    fn schema(&self) -> &Value {
        &self.schema
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffect
    }

    async fn invoke(&self, _ctx: &ToolContext<Ctx>, args: Value) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
            schema_errors: vec![e.to_string()],
        })?;
        self.check_command_allowed(&args.command)?;

        let mut cmd = build_command(&args.command);
        cmd.current_dir(self.sandbox.root())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in &self.env_allowlist {
            if let Ok(val) = std::env::var(name) {
                cmd.env(name, val);
            }
        }
        #[cfg(unix)]
        {
            // New process group so a timeout can kill the whole group.
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::Other(anyhow::anyhow!("failed to spawn shell: {e}")))?;

        // Capture the pid (== the process-group id, because `process_group(0)`
        // above made the child its own group leader) BEFORE reaping it, so a
        // timeout kill can target the whole group with no PID-reuse race.
        #[cfg(unix)]
        let pgid = child.id();

        let stdout_pipe = child.stdout.take().expect("piped stdout");
        let stderr_pipe = child.stderr.take().expect("piped stderr");

        let cap = self.max_output_bytes;
        // Drain both pipes concurrently with the wait so a child cannot block
        // on a full pipe buffer (classic deadlock).
        let out_handle = tokio::spawn(read_capped(stdout_pipe, cap));
        let err_handle = tokio::spawn(read_capped(stderr_pipe, cap));

        let mut timed_out = false;
        let exit_code: Option<i32>;
        match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(status) => {
                exit_code = status.map_err(|e| ToolError::Other(e.into()))?.code();
            }
            Err(_) => {
                timed_out = true;
                // Kill the WHOLE process group (unix) so backgrounded
                // grandchildren die too and release the stdout/stderr pipes.
                // The child is not yet reaped, so `pgid` is still valid.
                #[cfg(unix)]
                {
                    if let Some(pid) = pgid {
                        // pid < 4_194_304 on all supported platforms, so the
                        // `as i32` cast and negation are safe. A kill error
                        // here (e.g. ESRCH if the group already exited) is
                        // benign — the bounded wait/drain below still returns.
                        let _ = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
                    }
                }
                #[cfg(not(unix))]
                {
                    // No portable group kill; best-effort direct-child kill.
                    let _ = child.start_kill();
                }
                // Bounded reap so an uninterruptible (D-state) process can't
                // hang the invocation.
                exit_code = match tokio::time::timeout(GRACE, child.wait()).await {
                    Ok(Ok(status)) => status.code(),
                    _ => None,
                };
            }
        }

        // Join the reader tasks, BOUNDED: on the success path a leaked
        // backgrounded process (`cmd &`) can hold the pipe open, so we give the
        // readers a grace period then abort and return what we captured. This
        // guarantees `invoke` always returns.
        let ((stdout, out_trunc), (stderr, err_trunc)) =
            tokio::join!(join_reader(out_handle), join_reader(err_handle));

        Ok(ToolOutput::new(serde_json::json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": exit_code,
            "timed_out": timed_out,
            "truncated": out_trunc || err_trunc,
        })))
    }
}

/// Build the platform shell command without yet setting cwd/env.
fn build_command(command: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    }
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
}

/// Await a spawned reader task, bounded by [`GRACE`]. If it does not finish in
/// time (e.g. a leaked background process is holding the pipe open), abort it
/// and return empty output rather than hanging the invocation.
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

/// Read up to `cap` bytes from `pipe` as lossy UTF-8; the bool is `true` if the
/// output was truncated at the cap. Takes the pipe by value so the spawned
/// reader task owns it and dropping it closes the underlying fd. I/O errors are
/// ignored — whatever was buffered up to that point is returned.
async fn read_capped<R>(pipe: R, cap: usize) -> (String, bool)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut buf = Vec::new();
    // Read a little past the cap so we can detect truncation, then trim.
    let _ = pipe.take((cap as u64) + 1).read_to_end(&mut buf).await;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

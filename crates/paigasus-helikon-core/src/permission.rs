//! Permission layer: gate tool calls via deny rules → permission mode →
//! `canUseTool` policy. See the *Permissions, Guardrails & Hooks* concept page.

use async_trait::async_trait;

use crate::RunContext;

/// How permission mode governs tool calls.
///
/// Transitions are **tighten-only**, enforced by
/// [`RunContext::with_permission_mode`]: `Bypass` never loosens (it may only
/// tighten to `DontAsk`), and `DontAsk` is terminal. Both propagate to
/// subagents — a typed enum, not a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PermissionMode {
    /// Defer to the policy (ask for unfamiliar tools); permissive when no policy.
    #[default]
    Default,
    /// Auto-approve tools with a [`crate::ToolEffect::Write`] effect; all other
    /// tools still reach the policy.
    AcceptEdits,
    /// Read-only: deny any tool whose [`crate::ToolEffect`] is not `ReadOnly`.
    Plan,
    /// Dangerous: allow all (deny rules still apply). Propagates; sticky.
    Bypass,
    /// Locked-down headless inverse of `Bypass`: deny-by-default. The policy
    /// (`canUseTool`) is never invoked; only an [`crate::AllowRule`] (after
    /// the deny+guard steps) can permit a call. Sticky and terminal — once set
    /// it cannot be loosened.
    DontAsk,
}

/// The outcome of a [`PermissionPolicy::check`] (or the resolved decision).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PermissionDecision {
    /// Run the call unchanged.
    Allow,
    /// Block the call; the reason is surfaced to the model as a tool result.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
    /// Ask a human (resolved via [`ApprovalHandler`]; default Deny).
    AskUser {
        /// Prompt shown to the approver.
        prompt: String,
    },
    /// Replace the call's arguments before execution (sanitize).
    Replace {
        /// Replacement JSON arguments.
        args: serde_json::Value,
    },
}

/// Authorizes a tool call. The decision pipeline runs
/// `deny rules › guard rules › allow rules › mode › this policy` (see
/// `control.rs`).
#[async_trait]
pub trait PermissionPolicy<Ctx>: Send + Sync
where
    Ctx: Send + Sync + 'static,
{
    /// Decide whether `tool` may run with `args`.
    async fn check(
        &self,
        ctx: &RunContext<Ctx>,
        tool: &str,
        args: &serde_json::Value,
    ) -> PermissionDecision;
}

/// How a [`DenyRule`] matches a call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Matcher {
    /// Exact tool name.
    Tool(String),
    /// Any Bash sub-command whose resolved program equals this. Tool-scoped to
    /// the `Bash` tool.
    BashProgram(String),
    /// `Read` tool whose `path` arg matches this glob.
    ReadPath(crate::path_match::PathGlob),
    /// `Edit`/`Write` tool whose `path` arg matches this glob.
    EditPath(crate::path_match::PathGlob),
}

/// A first-class deny rule, evaluated **before** mode — so it overrides even
/// [`PermissionMode::Bypass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyRule {
    matcher: Matcher,
}

impl DenyRule {
    /// Deny a tool by its exact name.
    pub fn tool(name: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::Tool(name.into()),
        }
    }

    /// Deny a Bash call whose compound command contains a sub-command whose
    /// resolved program equals `program` (operator-, wrapper-, and
    /// `bash -c`-aware). Only matches the `Bash` tool.
    pub fn bash_command(program: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::BashProgram(program.into()),
        }
    }

    /// Deny a `Read` whose `path` arg matches `pattern` (gitignore-style glob:
    /// no `/` matches at any depth, a `/` anchors to the path root,
    /// case-insensitive). Scoped to the `Read` tool only — does **not** match
    /// `Edit`/`Write`. Lexical and **advisory** — not a sandbox.
    pub fn read(pattern: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::ReadPath(crate::path_match::PathGlob::new(pattern)),
        }
    }

    /// Deny an `Edit`/`Write` whose `path` arg matches `pattern`.
    pub fn edit(pattern: impl Into<String>) -> Self {
        Self {
            matcher: Matcher::EditPath(crate::path_match::PathGlob::new(pattern)),
        }
    }

    /// `true` if this rule denies `tool` invoked with `args`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        match &self.matcher {
            Matcher::Tool(name) => name == tool,
            Matcher::BashProgram(program) => {
                if tool != "Bash" {
                    return false;
                }
                let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                    return false;
                };
                crate::command_match::resolve_all(command)
                    .iter()
                    .any(|c| &c.program == program)
            }
            Matcher::ReadPath(glob) => path_arg_matches(tool, args, PathKind::Read, glob),
            Matcher::EditPath(glob) => path_arg_matches(tool, args, PathKind::Edit, glob),
        }
    }
}

/// Which tool family a path-rule applies to.
#[derive(Clone, Copy)]
enum PathKind {
    Read,
    Edit,
}

/// `true` if `tool` is in `kind`'s family and its `path` arg matches `glob`.
/// `Read` → tool `"Read"`; `Edit` → tools `"Edit"` and `"Write"`.
fn path_arg_matches(
    tool: &str,
    args: &serde_json::Value,
    kind: PathKind,
    glob: &crate::path_match::PathGlob,
) -> bool {
    let applies = match kind {
        PathKind::Read => tool == "Read",
        PathKind::Edit => tool == "Edit" || tool == "Write",
    };
    if !applies {
        return false;
    }
    args.get("path")
        .and_then(|v| v.as_str())
        .map(|p| glob.matches_path(p))
        .unwrap_or(false)
}

/// How an [`AllowRule`] matches a call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AllowMatcher {
    /// Exact tool name.
    Tool(String),
    /// Bash call where **every** sub-command's program equals this (fail-closed).
    BashProgram(String),
    /// `Read` tool whose `path` arg matches this glob.
    ReadPath(crate::path_match::PathGlob),
    /// `Edit`/`Write` tool whose `path` arg matches this glob.
    EditPath(crate::path_match::PathGlob),
}

/// A positive permission rule: a **global, all-modes, per-tool/per-command
/// pre-approval**. When an allow rule matches, the call is allowed in *every*
/// mode and `canUseTool` is **not** consulted for it (the deny and guard steps
/// still run first). Prefer [`AllowRule::bash_command`] over
/// `AllowRule::tool("Bash")` so a single allowed program does not disable all
/// Bash policy checks. Evaluated after deny + guard, before mode (see
/// `control.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRule {
    matcher: AllowMatcher,
}

impl AllowRule {
    /// Allow a tool by its exact name.
    pub fn tool(name: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::Tool(name.into()),
        }
    }

    /// Allow a Bash call **only when every** resolved sub-command's program
    /// equals `program` (operator-, wrapper-, `bash -c`-aware). A compound
    /// command with any other program does not match (fail-closed). Only the
    /// `Bash` tool. v1 does not compose multiple `bash_command` allows across a
    /// single compound command.
    pub fn bash_command(program: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::BashProgram(program.into()),
        }
    }

    /// Allow a `Read` whose `path` arg matches `pattern` (same gitignore-style
    /// glob semantics as [`DenyRule::read`]). Scoped to the `Read` tool only —
    /// does **not** match `Edit`/`Write`. Advisory, not a sandbox.
    pub fn read(pattern: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::ReadPath(crate::path_match::PathGlob::new(pattern)),
        }
    }

    /// Allow an `Edit`/`Write` whose `path` arg matches `pattern`.
    pub fn edit(pattern: impl Into<String>) -> Self {
        Self {
            matcher: AllowMatcher::EditPath(crate::path_match::PathGlob::new(pattern)),
        }
    }

    /// `true` if this rule allows `tool` invoked with `args`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        match &self.matcher {
            AllowMatcher::Tool(name) => name == tool,
            AllowMatcher::BashProgram(program) => {
                if tool != "Bash" {
                    return false;
                }
                let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
                    return false;
                };
                let subs = crate::command_match::resolve_all(command);
                !subs.is_empty() && subs.iter().all(|c| &c.program == program)
            }
            AllowMatcher::ReadPath(glob) => path_arg_matches(tool, args, PathKind::Read, glob),
            AllowMatcher::EditPath(glob) => path_arg_matches(tool, args, PathKind::Edit, glob),
        }
    }
}

/// Resolves a [`PermissionDecision::AskUser`] when the driver cannot decide
/// inline. Non-generic — it needs no `Ctx`.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    /// Decide an `AskUser` prompt. Returns a narrowed [`ApprovalOutcome`]
    /// (cannot recursively ask).
    async fn decide(&self, tool: &str, prompt: &str, args: &serde_json::Value) -> ApprovalOutcome;
}

/// The narrowed decision an [`ApprovalHandler`] may return.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ApprovalOutcome {
    /// Allow the call.
    Allow,
    /// Deny the call with a reason.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
}

/// The action a tripped [`GuardRule`] takes. Evaluated **before** mode, so it
/// overrides even [`PermissionMode::Bypass`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardAction {
    /// Hard-deny with a reason.
    Deny {
        /// Human-readable denial reason.
        reason: String,
    },
    /// Ask a human via the [`ApprovalHandler`] (default Deny when none).
    Ask {
        /// Prompt shown to the approver.
        prompt: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GuardMatcher {
    /// `rm` with recursive+force flags targeting `/` or `~` (literal).
    RmRecursiveRootOrHome,
    /// A write whose target resolves under a protected prefix (Bash redirects,
    /// `tee`/`dd`, or the Write/Edit `path` arg). Honors the device-node allowlist.
    ProtectedPathWrite,
    /// A write whose target has a `.git`/`.ssh` path component or a `.env`(`.env.*`)
    /// final component (Bash redirects, `tee`/`dd`, or the Write/Edit `path` arg).
    ProtectedDotPathWrite,
}

/// A pre-mode safety rule. Like [`DenyRule`] it runs before permission mode and
/// beats `Bypass`, but it may **ask** a human instead of hard-denying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardRule {
    matcher: GuardMatcher,
    action: GuardAction,
}

/// Protected path prefixes. A write resolving under any of these trips
/// [`GuardMatcher::ProtectedPathWrite`].
const PROTECTED_PREFIXES: &[&str] = &["/etc", "/usr", "/bin", "/sbin", "/sys", "/boot", "/dev"];

/// Device nodes that are safe write targets despite the `/dev` prefix. Checked
/// before the protected-prefix rule so `cmd > /dev/null` is never denied.
const DEVICE_ALLOWLIST: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/tty",
    "/dev/random",
    "/dev/urandom",
];

impl GuardRule {
    /// The action this rule takes when it matches.
    pub fn action(&self) -> &GuardAction {
        &self.action
    }

    /// `true` if this guard trips for `tool` invoked with `args`.
    pub fn matches(&self, tool: &str, args: &serde_json::Value) -> bool {
        match &self.matcher {
            GuardMatcher::RmRecursiveRootOrHome => {
                let Some(cmd) = bash_command_str(tool, args) else {
                    return false;
                };
                crate::command_match::resolve_all(cmd)
                    .iter()
                    .any(is_rm_rf_root_or_home)
            }
            GuardMatcher::ProtectedPathWrite => protected_path_write(tool, args),
            GuardMatcher::ProtectedDotPathWrite => protected_dotpath_write(tool, args),
        }
    }

    /// The always-on destructive guard set: `rm -rf /`, `rm -rf ~`, and
    /// protected-path writes. All default to [`GuardAction::Ask`].
    pub fn destructive_defaults() -> Vec<GuardRule> {
        vec![
            GuardRule {
                matcher: GuardMatcher::RmRecursiveRootOrHome,
                action: GuardAction::Ask {
                    prompt: "destructive command: recursive force-remove of / or ~".to_owned(),
                },
            },
            GuardRule {
                matcher: GuardMatcher::ProtectedPathWrite,
                action: GuardAction::Ask {
                    prompt: "write to a protected system path".to_owned(),
                },
            },
            GuardRule {
                matcher: GuardMatcher::ProtectedDotPathWrite,
                action: GuardAction::Ask {
                    prompt: "write to a protected VCS/secret path (.git, .ssh, .env)".to_owned(),
                },
            },
        ]
    }
}

fn bash_command_str<'a>(tool: &str, args: &'a serde_json::Value) -> Option<&'a str> {
    if tool != "Bash" {
        return None;
    }
    args.get("command").and_then(|v| v.as_str())
}

fn is_rm_rf_root_or_home(cmd: &crate::command_match::ResolvedCommand) -> bool {
    if cmd.program != "rm" {
        return false;
    }
    let mut recursive = false;
    let mut force = false;
    let mut targets: Vec<&str> = Vec::new();
    for a in &cmd.args {
        if a.starts_with("--") {
            match a.as_str() {
                "--recursive" => recursive = true,
                "--force" => force = true,
                _ => {}
            }
        } else if let Some(flags) = a.strip_prefix('-') {
            if flags.contains('r') || flags.contains('R') {
                recursive = true;
            }
            if flags.contains('f') {
                force = true;
            }
        } else {
            targets.push(a);
        }
    }
    recursive && force && targets.iter().any(|t| is_root_or_home(t))
}

fn is_root_or_home(target: &str) -> bool {
    matches!(target, "/" | "/*" | "~" | "~/" | "${HOME}" | "$HOME")
}

fn protected_path_write(tool: &str, args: &serde_json::Value) -> bool {
    if matches!(tool, "Write" | "Edit") {
        if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
            return is_protected_path(p);
        }
    }
    if let Some(cmd) = bash_command_str(tool, args) {
        for c in crate::command_match::resolve_all(cmd) {
            for r in &c.redirects {
                use crate::command_match::RedirectOp;
                if matches!(r.op, RedirectOp::Out | RedirectOp::Append)
                    && is_protected_path(&r.target)
                {
                    return true;
                }
            }
            if c.program == "tee" && c.args.iter().any(|a| is_protected_path(a)) {
                return true;
            }
            if c.program == "dd" {
                if let Some(of) = c.args.iter().find_map(|a| a.strip_prefix("of=")) {
                    if is_protected_path(of) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn protected_dotpath_write(tool: &str, args: &serde_json::Value) -> bool {
    if matches!(tool, "Write" | "Edit") {
        if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
            if crate::path_match::is_protected_dotpath(p) {
                return true;
            }
        }
    }
    if let Some(cmd) = bash_command_str(tool, args) {
        for c in crate::command_match::resolve_all(cmd) {
            for r in &c.redirects {
                use crate::command_match::RedirectOp;
                if matches!(r.op, RedirectOp::Out | RedirectOp::Append)
                    && crate::path_match::is_protected_dotpath(&r.target)
                {
                    return true;
                }
            }
            if c.program == "tee"
                && c.args
                    .iter()
                    .any(|a| crate::path_match::is_protected_dotpath(a))
            {
                return true;
            }
            if c.program == "dd" {
                if let Some(of) = c.args.iter().find_map(|a| a.strip_prefix("of=")) {
                    if crate::path_match::is_protected_dotpath(of) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn is_protected_path(path: &str) -> bool {
    if DEVICE_ALLOWLIST.contains(&path) {
        return false;
    }
    if path == "/" {
        return true;
    }
    PROTECTED_PREFIXES
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    use serde_json::json;

    fn matched(cmd: &str) -> bool {
        let bash = json!({ "command": cmd });
        GuardRule::destructive_defaults()
            .iter()
            .any(|g| g.matches("Bash", &bash))
    }

    #[test]
    fn matches_rm_rf_root_and_home() {
        assert!(matched("rm -rf /"));
        assert!(matched("rm -rf ~"));
        assert!(matched("rm -fr /"));
        assert!(matched("sudo rm -rf /"));
        assert!(matched("bash -c 'rm -rf /'"));
        assert!(matched("rm -rf / tmp")); // spacing bug
    }

    #[test]
    fn ignores_safe_rm() {
        assert!(!matched("rm -rf ./build"));
        assert!(!matched("rm file.txt"));
    }

    #[test]
    fn matches_protected_path_write_but_allows_dev_null() {
        assert!(matched("echo x > /etc/passwd"));
        assert!(matched("echo x >/etc/passwd"));
        assert!(matched("tee /etc/hosts"));
        assert!(!matched("echo x > /dev/null"));
        assert!(!matched("cmd 2> /dev/null"));
    }

    #[test]
    fn protected_path_write_matches_write_tool_path_arg() {
        let g = GuardRule::destructive_defaults();
        let write = json!({ "path": "/etc/passwd", "content": "x" });
        assert!(g.iter().any(|r| r.matches("Write", &write)));
        let safe = json!({ "path": "./notes.txt", "content": "x" });
        assert!(!g.iter().any(|r| r.matches("Write", &safe)));
    }

    #[test]
    fn destructive_defaults_use_ask_action() {
        assert!(GuardRule::destructive_defaults()
            .iter()
            .all(|g| matches!(g.action(), GuardAction::Ask { .. })));
    }

    #[test]
    fn matches_protected_dotpath_write() {
        // Write/Edit tool path arg
        let g = GuardRule::destructive_defaults();
        assert!(g
            .iter()
            .any(|r| r.matches("Write", &json!({ "path": ".git/config", "content": "x" }))));
        assert!(g
            .iter()
            .any(|r| r.matches("Edit", &json!({ "path": "a/.ssh/known_hosts" }))));
        assert!(g
            .iter()
            .any(|r| r.matches("Write", &json!({ "path": ".env.local", "content": "x" }))));
        // bare repo / lookalikes do NOT trip
        assert!(!g
            .iter()
            .any(|r| r.matches("Write", &json!({ "path": "repo.git/HEAD", "content": "x" }))));
        assert!(!g
            .iter()
            .any(|r| r.matches("Write", &json!({ "path": ".gitignore", "content": "x" }))));
        // bash redirect into .git
        assert!(matched("echo x > .git/config"));
        assert!(matched("echo x | tee .ssh/authorized_keys"));
        assert!(!matched("echo x > notes.txt"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_mode_default_is_default_variant() {
        assert_eq!(PermissionMode::default(), PermissionMode::Default);
    }

    #[test]
    fn deny_rule_read_edit_path_variants() {
        let dr = DenyRule::read(".env");
        assert!(dr.matches("Read", &json!({ "path": "config/.env" })));
        assert!(!dr.matches("Read", &json!({ "path": "config/app.toml" })));
        assert!(!dr.matches("Edit", &json!({ "path": ".env" }))); // read-scoped

        let de = DenyRule::edit("src/**");
        assert!(de.matches("Edit", &json!({ "path": "src/a.rs" })));
        assert!(de.matches("Write", &json!({ "path": "src/a.rs" }))); // edit covers Write
        assert!(!de.matches("Read", &json!({ "path": "src/a.rs" })));
    }

    #[test]
    fn allow_rule_tool_and_path() {
        assert!(AllowRule::tool("WebSearch").matches("WebSearch", &json!({})));
        assert!(!AllowRule::tool("WebSearch").matches("Bash", &json!({})));

        assert!(AllowRule::read("src/**").matches("Read", &json!({ "path": "src/a.rs" })));
        assert!(AllowRule::edit("src/**").matches("Write", &json!({ "path": "src/a.rs" })));
        assert!(!AllowRule::edit("src/**").matches("Write", &json!({ "path": "etc/x" })));
        // read is scoped to Read — it does NOT cover Write/Edit
        assert!(!AllowRule::read("src/**").matches("Write", &json!({ "path": "src/a.rs" })));
        // a missing/non-string path arg matches nothing (no panic)
        assert!(!AllowRule::read("**").matches("Read", &json!({})));
        assert!(!DenyRule::read("**").matches("Read", &json!({})));
    }

    #[test]
    fn allow_rule_bash_command_requires_every_subcommand() {
        let rule = AllowRule::bash_command("git");
        assert!(rule.matches("Bash", &json!({ "command": "git status && git push" })));
        // a non-git sub-command means the allow rule does NOT fire (fail-closed)
        assert!(!rule.matches("Bash", &json!({ "command": "git status && rm -rf ." })));
        assert!(!rule.matches("Bash", &json!({ "command": "" })));
        assert!(!rule.matches("Other", &json!({ "command": "git status" })));
    }

    #[test]
    fn deny_rule_matches_exact_tool_name_only() {
        let rule = DenyRule::tool("rm");
        assert!(rule.matches("rm", &json!({})));
        assert!(!rule.matches("ls", &json!({})));
        assert!(rule.matches("rm", &json!({"path": "/etc/passwd"})));
    }

    #[test]
    fn bash_command_matches_any_subcommand_program() {
        let rule = DenyRule::bash_command("rm");
        let args = json!({ "command": "echo ok && rm -rf ." });
        assert!(rule.matches("Bash", &args));
        let safe = json!({ "command": "echo ok && ls" });
        assert!(!rule.matches("Bash", &safe));
    }

    #[test]
    fn bash_command_is_tool_scoped() {
        let rule = DenyRule::bash_command("rm");
        // A non-Bash tool carrying a `command` field must not trip it.
        assert!(!rule.matches("Other", &json!({ "command": "rm -rf ." })));
    }

    #[test]
    fn bash_command_sees_through_sudo_and_bash_c() {
        let rule = DenyRule::bash_command("rm");
        assert!(rule.matches("Bash", &json!({ "command": "sudo rm -rf /" })));
        assert!(rule.matches("Bash", &json!({ "command": "bash -c 'rm -rf /'" })));
    }
}

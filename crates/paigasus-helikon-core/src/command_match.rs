//! Pure, dependency-free shell-command tokenizer used by the permission layer
//! to match Bash sub-commands. Models Claude Code's pragmatic Bash matcher —
//! not a full POSIX shell grammar. See the crate's permissions concept page for
//! the enumerated coverage and known bypasses.

/// Split a compound command on shell control operators (`&&`, `||`, `;`, `|`,
/// `|&`, `&`, newlines), quote- and redirection-aware. The `&` of a redirection
/// (`2>&1`, `>&2`, `&>file`) is never treated as a control operator, and
/// operators inside `'…'` / `"…"` are ignored. Empty segments are dropped.
pub fn split_operators(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let n = bytes.len();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;
    let (mut in_single, mut in_double) = (false, false);

    while i < n {
        let c = bytes[i];
        if in_single {
            if c == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == b'\\' && i + 1 < n {
                i += 2; // skip the escaped char (e.g. \")
                continue;
            }
            if c == b'"' {
                in_double = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => {
                in_single = true;
                i += 1;
            }
            b'"' => {
                in_double = true;
                i += 1;
            }
            b'\\' => {
                i += 2;
            }
            b'\n' | b';' => {
                push_segment(command, start, i, &mut segments);
                i += 1;
                start = i;
            }
            b'&' => {
                if i + 1 < n && bytes[i + 1] == b'&' {
                    push_segment(command, start, i, &mut segments);
                    i += 2;
                    start = i;
                } else if (i > 0 && bytes[i - 1] == b'>') || (i + 1 < n && bytes[i + 1] == b'>') {
                    i += 1; // part of a redirection (>&, &>)
                } else {
                    push_segment(command, start, i, &mut segments);
                    i += 1;
                    start = i;
                }
            }
            b'|' => {
                let span = if i + 1 < n && (bytes[i + 1] == b'|' || bytes[i + 1] == b'&') {
                    2
                } else {
                    1
                };
                push_segment(command, start, i, &mut segments);
                i += span;
                start = i;
            }
            _ => {
                i += 1;
            }
        }
    }
    push_segment(command, start, n, &mut segments);
    segments
}

fn push_segment<'a>(s: &'a str, start: usize, end: usize, out: &mut Vec<&'a str>) {
    let seg = s[start..end].trim(); // end is always <= s.len()
    if !seg.is_empty() {
        out.push(seg);
    }
}

/// A parsed redirection (only the kinds the guard layer cares about).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectOp {
    /// `>` / `N>` / `&>` — truncating write to a path.
    Out,
    /// `>>` / `N>>` / `&>>` — appending write to a path.
    Append,
    /// `>&` / `N>&M` — file-descriptor duplication (no path target).
    FdDup,
}

/// One redirection of a sub-command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// The redirection kind.
    pub op: RedirectOp,
    /// The (unquoted) target. Empty for `FdDup`.
    pub target: String,
}

/// A single sub-command after wrapper-stripping and quote removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommand {
    /// The effective program token (unquoted/unescaped, wrappers removed).
    pub program: String,
    /// Remaining argument tokens (unquoted), redirections excluded.
    pub args: Vec<String>,
    /// Parsed redirections.
    pub redirects: Vec<Redirect>,
}

/// Maximum `bash -c` / `sh -c` re-entry depth the matcher follows.
pub const MAX_REENTRY_DEPTH: usize = 3;

const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash"];

/// If `cmd` invokes a known shell with a `-c <string>` argument, return the
/// inner command string for re-parsing.
pub fn shell_c_payload(cmd: &ResolvedCommand) -> Option<&str> {
    if !SHELLS.contains(&cmd.program.as_str()) {
        return None;
    }
    let mut it = cmd.args.iter();
    while let Some(a) = it.next() {
        if a == "-c" {
            return it.next().map(String::as_str);
        }
        if let Some(rest) = a.strip_prefix("-c") {
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

/// Split a compound command and resolve every sub-command, following
/// `bash -c` / `sh -c` re-entry up to [`MAX_REENTRY_DEPTH`].
pub fn resolve_all(command: &str) -> Vec<ResolvedCommand> {
    let mut out = Vec::new();
    resolve_into(command, 0, &mut out);
    out
}

fn resolve_into(command: &str, depth: usize, out: &mut Vec<ResolvedCommand>) {
    for seg in split_operators(command) {
        if let Some(cmd) = resolve_command(seg) {
            if depth < MAX_REENTRY_DEPTH {
                if let Some(inner) = shell_c_payload(&cmd) {
                    let inner = inner.to_owned();
                    out.push(cmd);
                    resolve_into(&inner, depth + 1, out);
                    continue;
                }
            }
            out.push(cmd);
        }
    }
}

/// Wrappers stripped from the front of a sub-command before resolving the
/// program. After the wrapper we skip a leading run of option tokens (`-x`) and
/// bare numeric "value"/duration tokens (e.g. `timeout 5`, `nice -n 10`).
const WRAPPERS: &[&str] = &[
    "timeout", "nice", "nohup", "stdbuf", "env", "command", "sudo", "doas",
];

/// Resolve one segment (already split by [`split_operators`]) into its effective
/// program, args, and redirections. Returns `None` for an empty segment.
pub fn resolve_command(segment: &str) -> Option<ResolvedCommand> {
    let (mut words, redirects) = tokenize(segment);
    loop {
        let first = words.first()?;
        if is_env_assignment(first) {
            words.remove(0);
            continue;
        }
        if WRAPPERS.contains(&first.as_str()) {
            let wrapper = words.remove(0);
            // Short options that take a SEPARATE argument, per wrapper. After
            // such an option we also skip the following non-option token, so the
            // wrapper's option-value is never mistaken for the program.
            let arg_opts: &[&str] = match wrapper.as_str() {
                "sudo" | "doas" => &["-u", "-g", "-C", "-h", "-p", "-r", "-t", "-U", "-R", "-c"],
                "timeout" => &["-s", "-k"],
                "nice" => &["-n"],
                "stdbuf" => &["-i", "-o", "-e"],
                "env" => &["-u", "-C", "-S"],
                _ => &[],
            };
            while let Some(w) = words.first() {
                let numeric = !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == '.');
                if w.starts_with('-') {
                    let takes_arg = arg_opts.contains(&w.as_str());
                    words.remove(0);
                    if takes_arg {
                        if let Some(v) = words.first() {
                            if !v.starts_with('-') {
                                words.remove(0);
                            }
                        }
                    }
                } else if numeric {
                    words.remove(0);
                } else {
                    break;
                }
            }
            continue;
        }
        break;
    }
    if words.is_empty() {
        return None;
    }
    let program = words.remove(0);
    Some(ResolvedCommand {
        program,
        args: words,
        redirects,
    })
}

fn is_env_assignment(tok: &str) -> bool {
    let Some(eq) = tok.find('=') else {
        return false;
    };
    let name = &tok[..eq];
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Split a single segment into words and redirections, quote-aware.
/// ASCII-pragmatic: multibyte content only ever appears inside quoted args,
/// which never affect program/redirect detection.
fn tokenize(segment: &str) -> (Vec<String>, Vec<Redirect>) {
    let bytes = segment.as_bytes();
    let n = bytes.len();
    let mut words = Vec::new();
    let mut redirects = Vec::new();
    let mut i = 0;

    while i < n {
        while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut j = i;
        while j < n && bytes[j].is_ascii_digit() {
            j += 1;
        }
        let amp = bytes[i] == b'&' && i + 1 < n && bytes[i + 1] == b'>';
        if amp || (j < n && (bytes[j] == b'>' || bytes[j] == b'<')) {
            let (redir, next) = parse_redirect(segment, i);
            if let Some(r) = redir {
                redirects.push(r);
            }
            i = next;
            continue;
        }
        let (word, next) = read_word(segment, i);
        if !word.is_empty() {
            words.push(word);
        }
        i = if next > i { next } else { i + 1 };
    }
    (words, redirects)
}

/// Parse a redirection starting at `start`. Returns the redirect (`None` for `<`
/// input redirections, which the guard layer ignores) and the next index.
fn parse_redirect(s: &str, start: usize) -> (Option<Redirect>, usize) {
    let bytes = s.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'&') {
        i += 1; // `&>`
    }
    if bytes.get(i) == Some(&b'<') {
        i += 1;
        let (_t, next) = read_redirect_target(s, i);
        return (None, next);
    }
    let mut op = RedirectOp::Out;
    if bytes.get(i) == Some(&b'>') {
        i += 1;
        if bytes.get(i) == Some(&b'>') {
            op = RedirectOp::Append;
            i += 1;
        } else if bytes.get(i) == Some(&b'&') {
            op = RedirectOp::FdDup;
            i += 1;
        }
    }
    if op == RedirectOp::FdDup {
        let (_t, next) = read_redirect_target(s, i);
        return (
            Some(Redirect {
                op,
                target: String::new(),
            }),
            next,
        );
    }
    let (target, next) = read_redirect_target(s, i);
    if target.is_empty() {
        return (None, next);
    }
    (Some(Redirect { op, target }), next)
}

/// Read a redirection target: skip spaces, then read one (possibly quoted) word.
fn read_redirect_target(s: &str, start: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = start;
    while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    read_word(s, i)
}

/// Read one whitespace-delimited word, removing quotes and backslash escapes.
fn read_word(s: &str, start: usize) -> (String, usize) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = start;
    let mut out = String::new();
    while i < n {
        match bytes[i] {
            b' ' | b'\t' | b'>' | b'<' => break,
            b'\'' => {
                i += 1;
                while i < n && bytes[i] != b'\'' {
                    out.push(bytes[i] as char);
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < n && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < n {
                        i += 1;
                    }
                    out.push(bytes[i] as char);
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            }
            b'\\' => {
                if i + 1 < n {
                    out.push(bytes[i + 1] as char);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    (out, i)
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn prog(seg: &str) -> String {
        resolve_command(seg).unwrap().program
    }

    #[test]
    fn strips_env_assignments_and_wrappers() {
        assert_eq!(prog("FOO=bar rm x"), "rm");
        assert_eq!(prog("timeout 5 rm x"), "rm");
        assert_eq!(prog("nice -n 10 rm x"), "rm");
        assert_eq!(prog("sudo rm -rf /"), "rm");
        assert_eq!(prog("doas rm x"), "rm");
        assert_eq!(prog("env FOO=bar nohup stdbuf -oL rm x"), "rm");
    }

    #[test]
    fn keeps_args_after_program() {
        let r = resolve_command("rm -rf /").unwrap();
        assert_eq!(r.program, "rm");
        assert_eq!(r.args, vec!["-rf", "/"]);
        let r = resolve_command("sudo rm -rf /tmp/x").unwrap();
        assert_eq!(r.program, "rm");
        assert_eq!(r.args, vec!["-rf", "/tmp/x"]);
    }

    #[test]
    fn unquotes_and_unescapes_the_program_token() {
        assert_eq!(prog(r"\rm -rf /"), "rm");
        assert_eq!(prog("'rm' -rf /"), "rm");
        assert_eq!(prog(r"r''m -rf /"), "rm");
    }

    #[test]
    fn parses_redirection_targets_spaced_glued_and_quoted() {
        let r = resolve_command("echo x > /etc/passwd").unwrap();
        assert_eq!(r.program, "echo");
        assert_eq!(
            r.redirects,
            vec![Redirect {
                op: RedirectOp::Out,
                target: "/etc/passwd".into()
            }]
        );

        let r = resolve_command("echo x >/etc/passwd").unwrap();
        assert_eq!(
            r.redirects,
            vec![Redirect {
                op: RedirectOp::Out,
                target: "/etc/passwd".into()
            }]
        );

        let r = resolve_command("echo x >> \"/etc/passwd\"").unwrap();
        assert_eq!(
            r.redirects,
            vec![Redirect {
                op: RedirectOp::Append,
                target: "/etc/passwd".into()
            }]
        );

        // fd-dup is not a path target
        let r = resolve_command("cmd 2>&1").unwrap();
        assert!(r
            .redirects
            .iter()
            .all(|x| x.op != RedirectOp::Out && x.op != RedirectOp::Append));
    }

    #[test]
    fn empty_segment_is_none() {
        assert!(resolve_command("   ").is_none());
    }

    #[test]
    fn strips_wrapper_options_that_take_an_argument() {
        // sudo -u root must not let "root" become the program (security).
        assert_eq!(
            resolve_command("sudo -u root rm /etc/passwd")
                .unwrap()
                .program,
            "rm"
        );
        assert_eq!(resolve_command("doas -u root rm x").unwrap().program, "rm");
        assert_eq!(
            resolve_command("timeout -s KILL 5 rm x").unwrap().program,
            "rm"
        );
        // bare flags without a separate arg must NOT swallow the program
        assert_eq!(resolve_command("sudo -i rm x").unwrap().program, "rm");
    }

    #[test]
    fn empty_redirect_target_is_dropped() {
        let r = resolve_command("echo >").unwrap();
        assert!(r.redirects.is_empty());
    }
}

#[cfg(test)]
mod split_tests {
    use super::*;

    #[test]
    fn splits_on_control_operators() {
        assert_eq!(
            split_operators("echo ok && rm -rf ."),
            vec!["echo ok", "rm -rf ."]
        );
        assert_eq!(split_operators("a; b | c || d"), vec!["a", "b", "c", "d"]);
        assert_eq!(split_operators("a |& b"), vec!["a", "b"]);
        assert_eq!(split_operators("a &\nb"), vec!["a", "b"]);
    }

    #[test]
    fn does_not_split_redirection_ampersands() {
        // 2>&1, >&2, &>file must NOT split on their '&'.
        assert_eq!(split_operators("cmd 2>&1"), vec!["cmd 2>&1"]);
        assert_eq!(split_operators("cmd >&2"), vec!["cmd >&2"]);
        assert_eq!(split_operators("cmd &>file"), vec!["cmd &>file"]);
    }

    #[test]
    fn does_not_split_inside_quotes() {
        assert_eq!(split_operators("echo 'a && b'"), vec!["echo 'a && b'"]);
        assert_eq!(split_operators("echo \"x ; y\""), vec!["echo \"x ; y\""]);
    }

    #[test]
    fn backslash_escapes_an_operator() {
        assert_eq!(split_operators(r"echo a\;b"), vec![r"echo a\;b"]);
    }

    #[test]
    fn empty_and_whitespace_yield_no_segments() {
        assert!(split_operators("").is_empty());
        assert!(split_operators("   ").is_empty());
    }

    #[test]
    fn escaped_quote_inside_double_quotes_does_not_split() {
        assert_eq!(
            split_operators("echo \"foo\\\"bar; baz\""),
            vec!["echo \"foo\\\"bar; baz\""]
        );
    }
}

#[cfg(test)]
mod resolve_all_tests {
    use super::*;

    fn programs(cmd: &str) -> Vec<String> {
        resolve_all(cmd).into_iter().map(|c| c.program).collect()
    }

    #[test]
    fn flattens_compound_commands() {
        assert_eq!(programs("echo ok && rm -rf ."), vec!["echo", "rm"]);
    }

    #[test]
    fn recurses_into_shell_c() {
        assert!(programs("bash -c 'rm -rf /'").contains(&"rm".to_string()));
        assert!(programs("sh -c \"echo hi && rm x\"").contains(&"rm".to_string()));
    }

    #[test]
    fn recursion_is_depth_bounded() {
        // Deeply nested -c beyond MAX_REENTRY_DEPTH must terminate (not loop).
        let nested = "bash -c 'bash -c \"bash -c \\\"bash -c rm\\\"\"'";
        let _ = resolve_all(nested);
    }
}

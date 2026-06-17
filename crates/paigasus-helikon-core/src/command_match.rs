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
    let seg = s[start..end.min(s.len())].trim();
    if !seg.is_empty() {
        out.push(seg);
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
}

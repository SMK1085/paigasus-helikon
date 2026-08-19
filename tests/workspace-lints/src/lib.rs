//! Workspace-wide source lints for the Helikon repo.
//!
//! Internal, never published. See
//! `docs/superpowers/specs/2026-08-19-sma-543-tracing-target-design.md`.

/// One `target =` / `parent =` argument found inside a `tracing` macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offense {
    /// 1-based line number of the offending keyword.
    pub line: usize,
    /// Macro it appeared in, unqualified (e.g. `warn` for `tracing::warn!`).
    pub macro_name: String,
    /// Which keyword was misused: `target` or `parent`.
    pub keyword: String,
}

/// Macros where `target:` / `parent:` are macro syntax, not field names.
const TRACING_MACROS: &[&str] = &[
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "event",
    "span",
    "trace_span",
    "debug_span",
    "info_span",
    "warn_span",
    "error_span",
];

/// Keywords that must be introduced with `:` and never with `=`.
const KEYWORDS: &[&str] = &["target", "parent"];

/// Scan Rust source for `tracing` macro arguments written `target = …` or
/// `parent = …`, which the macros silently treat as ordinary fields.
///
/// Every top-level argument is inspected, not just the first: for the span and
/// event macros the correct syntax puts `target:` *before* the level or span
/// name, so the erroneous form is only reachable in a later position.
///
/// Comments and literals are blanked before scanning, so a macro written out
/// inside a comment, doc example or string is never flagged.
pub fn scan(src: &str) -> Vec<Offense> {
    let masked = mask_trivia(src);
    let b = &masked[..];
    let mut offenses = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'!' {
            i += 1;
            continue;
        }
        let Some(name) = ident_before(b, i) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if b.get(j) != Some(&b'(') {
            i += 1;
            continue;
        }
        if TRACING_MACROS.contains(&name) {
            collect_args(b, j, name, &mut offenses);
        }
        i = j + 1;
    }
    offenses
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The identifier immediately preceding `at`, if any. Stops at `:`, so a
/// qualified `tracing::warn!` yields `warn`.
fn ident_before(b: &[u8], at: usize) -> Option<&str> {
    let mut s = at;
    while s > 0 && is_ident_byte(b[s - 1]) {
        s -= 1;
    }
    if s == at {
        return None;
    }
    std::str::from_utf8(&b[s..at]).ok()
}

fn blank(out: &mut [u8], from: usize, to: usize) {
    for byte in &mut out[from..to] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Replace every byte inside a comment or literal with a space, preserving
/// length, byte offsets and newlines so offsets still map onto the original.
fn mask_trivia(src: &str) -> Vec<u8> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let start = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let start = i;
                let mut depth = 1usize;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, start, i);
            }
            b'r' | b'b' => match raw_or_byte_string_end(b, i) {
                Some(end) => {
                    blank(&mut out, i, end);
                    i = end;
                }
                None => i += 1,
            },
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                    } else if b[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, start, i);
            }
            b'\'' => match char_literal_end(b, i) {
                Some(end) => {
                    blank(&mut out, i, end);
                    i = end;
                }
                // A lifetime, not a literal — leave it alone.
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    out
}

/// End (exclusive) of a raw or byte string starting at `i`, if one does.
fn raw_or_byte_string_end(b: &[u8], i: usize) -> Option<usize> {
    // `bar` must not read as a byte string starting at its `r`.
    if i > 0 && is_ident_byte(b[i - 1]) {
        return None;
    }
    let mut j = i;
    if b[j] == b'b' {
        j += 1;
    }
    let raw = b.get(j) == Some(&b'r');
    if raw {
        j += 1;
        let hashes_start = j;
        while b.get(j) == Some(&b'#') {
            j += 1;
        }
        let hashes = j - hashes_start;
        if b.get(j) != Some(&b'"') {
            return None;
        }
        j += 1;
        while j < b.len() {
            if b[j] == b'"' {
                let mut k = j + 1;
                let mut seen = 0;
                while seen < hashes && b.get(k) == Some(&b'#') {
                    k += 1;
                    seen += 1;
                }
                if seen == hashes {
                    return Some(k);
                }
            }
            j += 1;
        }
        return Some(b.len());
    }
    if b[i] != b'b' || b.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
        } else if b[j] == b'"' {
            return Some(j + 1);
        } else {
            j += 1;
        }
    }
    Some(b.len())
}

/// End (exclusive) of a char literal starting at `i`, or `None` for a lifetime.
fn char_literal_end(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i + 1) == Some(&b'\\') {
        let mut j = i + 2;
        while j < b.len() && b[j] != b'\'' {
            j += 1;
        }
        return if j < b.len() { Some(j + 1) } else { None };
    }
    let mut j = i + 1;
    if j >= b.len() {
        return None;
    }
    j += 1;
    // Consume UTF-8 continuation bytes of a multi-byte char.
    while j < b.len() && b[j] & 0b1100_0000 == 0b1000_0000 {
        j += 1;
    }
    if b.get(j) == Some(&b'\'') {
        Some(j + 1)
    } else {
        None
    }
}

fn starts_with_ident(b: &[u8], at: usize, word: &str) -> bool {
    let w = word.as_bytes();
    b.len() >= at + w.len()
        && &b[at..at + w.len()] == w
        && !b.get(at + w.len()).is_some_and(|&c| is_ident_byte(c))
}

fn line_of(b: &[u8], at: usize) -> usize {
    b[..at].iter().filter(|&&c| c == b'\n').count() + 1
}

/// Walk one macro invocation's argument list, flagging any top-level argument
/// that opens with `target =` or `parent =`.
fn collect_args(b: &[u8], open: usize, macro_name: &str, out: &mut Vec<Offense>) {
    let mut k = open + 1;
    let mut depth = 0usize;
    let mut at_arg_start = true;
    while k < b.len() {
        let c = b[k];
        if c == b'(' || c == b'[' || c == b'{' {
            depth += 1;
            at_arg_start = false;
        } else if c == b')' || c == b']' || c == b'}' {
            if depth == 0 {
                return;
            }
            depth -= 1;
            at_arg_start = false;
        } else if c == b',' && depth == 0 {
            at_arg_start = true;
        } else if c.is_ascii_whitespace() {
            // Whitespace never ends an argument-start position.
        } else {
            if at_arg_start && depth == 0 {
                if let Some(kw) = KEYWORDS.iter().find(|kw| starts_with_ident(b, k, kw)) {
                    let mut m = k + kw.len();
                    while m < b.len() && b[m].is_ascii_whitespace() {
                        m += 1;
                    }
                    if b.get(m) == Some(&b'=') && b.get(m + 1) != Some(&b'=') {
                        out.push(Offense {
                            line: line_of(b, k),
                            macro_name: macro_name.to_owned(),
                            keyword: (*kw).to_owned(),
                        });
                    }
                }
            }
            at_arg_start = false;
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(usize, String, String)> {
        scan(src)
            .into_iter()
            .map(|o| (o.line, o.macro_name, o.keyword))
            .collect()
    }

    /// Every form that actually compiles against `tracing` 0.1 and silently
    /// records a field instead of setting the metadata target. Verified with
    /// rustc; see spec §4.4 for the compile matrix.
    #[test]
    fn flags_every_compiling_bad_form() {
        let cases: &[(&str, &str, &str)] = &[
            (r#"tracing::warn!(target = "x", "m");"#, "warn", "target"),
            (r#"warn!(target="x", "m");"#, "warn", "target"),
            (
                r#"tracing::warn!(parent: None, target = "x", "m");"#,
                "warn",
                "target",
            ),
            (
                r#"tracing::info_span!("nm", target = "x");"#,
                "info_span",
                "target",
            ),
            (
                r#"tracing::event!(Level::WARN, target = "x", "m");"#,
                "event",
                "target",
            ),
            (r#"tracing::info!(parent = p, "m");"#, "info", "parent"),
        ];
        for (src, mac, kw) in cases {
            let got = kinds(src);
            assert_eq!(
                got,
                vec![(1, (*mac).to_owned(), (*kw).to_owned())],
                "expected one offense for `{src}`"
            );
        }
    }

    /// The real shape of the SMA-543 bug: the keyword is on its own line.
    #[test]
    fn flags_the_multiline_shape_and_reports_the_keyword_line() {
        let src = "fn f() {\n    tracing::warn!(\n        target = \"paigasus::openai::translate\",\n        \"unknown Item variant; skipping\"\n    );\n}\n";
        assert_eq!(
            kinds(src),
            vec![(3, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// `cargo test --workspace --all-features` also runs on windows-latest.
    #[test]
    fn line_numbers_survive_crlf() {
        let src = "fn f() {\r\n    tracing::warn!(\r\n        target = \"x\",\r\n        \"m\"\r\n    );\r\n}\r\n";
        assert_eq!(
            kinds(src),
            vec![(3, "warn".to_owned(), "target".to_owned())]
        );
    }

    #[test]
    fn accepts_correct_and_unrelated_forms() {
        let cases = [
            r#"tracing::warn!(target: "x", "m");"#,
            r#"tracing::info_span!(parent: parent, "nm");"#,
            r#"tracing::info!(count = 1, "m");"#,
            r#"let target = "x";"#,
            r#"if a == b { let parent = 1; }"#,
            r#"foo!(target = "x");"#,
            r#"Thing { target: "x" }"#,
            r#"fn f<'a>(x: &'a str) { tracing::warn!(target: "t", "m"); }"#,
            r#"tracing::warn!(target: "t", other = compute(a, b), "m");"#,
        ];
        for src in cases {
            assert_eq!(kinds(src), vec![], "false positive on `{src}`");
        }
    }

    /// The guard scans its own source. Because the lexer blanks comments and
    /// literals, the bad forms in this very file are invisible to it — which
    /// is why there is deliberately no path-based self-exclusion (spec §4.5).
    #[test]
    fn ignores_comments_and_literals() {
        let cases = [
            r#"// tracing::warn!(target = "x", "m");"#,
            r#"/// tracing::warn!(target = "x", "m");"#,
            r#"/* tracing::warn!(target = "x", "m"); */"#,
            r#"/* outer /* nested */ tracing::warn!(target = "x"); */"#,
            r#"let s = "tracing::warn!(target = \"x\")";"#,
            "let s = r#\"tracing::warn!(target = \"x\")\"#;",
            r#"let s = b"warn!(target = 1)";"#,
            r#"let c = '"'; let d = '\''; "#,
        ];
        for src in cases {
            assert_eq!(kinds(src), vec![], "false positive on `{src}`");
        }
    }

    #[test]
    fn reports_every_offense_in_a_file() {
        let src = "tracing::warn!(target = \"a\", \"m\");\ntracing::warn!(target: \"ok\", \"m\");\ntracing::error!(target = \"b\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![
                (1, "warn".to_owned(), "target".to_owned()),
                (3, "error".to_owned(), "target".to_owned()),
            ]
        );
    }

    /// A string containing an unbalanced paren must not desynchronise the
    /// argument walk.
    #[test]
    fn unbalanced_paren_inside_a_literal_does_not_desync() {
        let src =
            "tracing::warn!(target: \"a)b\", \"m\");\ntracing::warn!(target = \"c\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![(2, "warn".to_owned(), "target".to_owned())]
        );
    }
}
